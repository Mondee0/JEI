// Jei-style item browser sidebar for Weft
// Shows a grid of items on the right side of inventory screens
// with pagination and search functionality. Clicking an item opens
// Weft's built-in recipe viewer. Star button toggles favorite selection.

use weft_sdk::*;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

declare_mod!(deps: ["base"], init: init);

const COLUMNS: usize = 5;
const ROWS: usize = 6;
const PER_PAGE: usize = COLUMNS * ROWS;
const SLOT: f32 = 22.0;
const GAP: f32 = 2.0;
const PAD: f32 = 4.0;
const GRID_W: f32 = COLUMNS as f32 * SLOT + (COLUMNS as f32 - 1.0) * GAP;
const PANEL_W: f32 = GRID_W + PAD * 2.0;
const WIDE_PANEL_W: f32 = GRID_W * 2.0 + GAP + PAD * 2.0;

static DUMMY: Menu = Menu::named("jei_sidebar_rebuild");
static FAV_TOGGLE_ELEMENT: HudElement = HudElement::named("jei_sidebar_fav_toggle");
static CRAFTING_KIND: RecipeKind = RecipeKind::named("base:crafting");
static CRAFTING_MENU: Menu = Menu::named("base:crafting_table");
static INVENTORY_MENU: Menu = Menu::named("base:inventory");
static QUICK_CRAFT: Channel<Vec<u8>> = Channel::named("quick_craft");
static PENDING_CRAFT: Mutex<Vec<(u64, Instant, QuickCraft)>> = Mutex::new(Vec::new());

static FAV_REVISION: AtomicUsize = AtomicUsize::new(0);
static TOGGLE_REVISION: AtomicUsize = AtomicUsize::new(0);
static PREV_LEFT_DOWN: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct State {
    search: String,
    page: usize,
    rebuild: bool,
    pending_recipe: u32,
    favorites: Vec<u32>,
    selecting: bool,
    fav_page: usize,
    show_favorites: bool,
    loaded: bool,
    last_screen: String,
}

static STATE: Mutex<State> = Mutex::new(State {
    search: String::new(),
    page: 0,
    rebuild: false,
    pending_recipe: 0,
    favorites: Vec::new(),
    selecting: false,
    fav_page: 0,
    show_favorites: true,
    loaded: false,
    last_screen: String::new(),
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(target_os = "windows")]
mod win {
    use core::ffi::c_void;

    #[repr(C)]
    pub struct Point {
        pub x: i32,
        pub y: i32,
    }

    #[repr(C)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn GetForegroundWindow() -> *mut c_void;
        pub fn GetClientRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
        pub fn ScreenToClient(hwnd: *mut c_void, point: *mut Point) -> i32;
        pub fn GetCursorPos(point: *mut Point) -> i32;
        pub fn GetAsyncKeyState(vkey: i32) -> i16;
        pub fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetCurrentProcessId() -> u32;
    }
}

/// The mouse in the game window's client area, and that area's size, in physical pixels.
#[cfg(target_os = "windows")]
fn cursor_in_game_client() -> Option<(f32, f32, f32, f32)> {
    unsafe {
        let hwnd = win::GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        win::GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != win::GetCurrentProcessId() {
            return None;
        }
        let mut p = win::Point { x: 0, y: 0 };
        if win::GetCursorPos(&mut p) == 0 || win::ScreenToClient(hwnd, &mut p) == 0 {
            return None;
        }
        let mut r = win::Rect { left: 0, top: 0, right: 0, bottom: 0 };
        if win::GetClientRect(hwnd, &mut r) == 0 {
            return None;
        }
        Some((
            p.x as f32,
            p.y as f32,
            (r.right - r.left).max(1) as f32,
            (r.bottom - r.top).max(1) as f32,
        ))
    }
}

#[cfg(target_os = "windows")]
fn left_mouse_down() -> bool {
    unsafe { (win::GetAsyncKeyState(0x01) as u16 & 0x8000) != 0 }
}

/// The hide/show button is a HUD element at the screen's bottom-right corner. The engine does
/// not route mouse clicks to HUD widgets, so hit-test that corner ourselves.
#[cfg(target_os = "windows")]
fn corner_button_click(c: &Client, screen: &str) {
    let down = left_mouse_down();
    let was = PREV_LEFT_DOWN.swap(down, Ordering::SeqCst);
    if !down || was {
        return;
    }
    if screen != "core:menu" || state().favorites.is_empty() {
        return;
    }
    let st = c.state();
    if st.gui_scale <= 0.0 || st.window[0] == 0 || st.window[1] == 0 {
        return;
    }
    let Some((cx, cy, cw, ch)) = cursor_in_game_client() else { return };
    // Map the client-area cursor onto the surface the HUD was laid out for.
    let x = cx * st.window[0] as f32 / cw;
    let y = cy * st.window[1] as f32 / ch;
    // The button sits at the BOTTOM_RIGHT HUD anchor (just above the hotbar, right side).
    // Use a generous hit box around its measured position so real clicks register.
    let bx = st.window[0] as f32 * 0.93;
    let by = st.window[1] as f32 * 0.85;
    let bsx = st.window[0] as f32 * 0.07;
    let bsy = st.window[1] as f32 * 0.12;
    if x >= bx && x <= bx + bsx && y >= by && y <= by + bsy {
        let mut s = state();
        s.show_favorites = !s.show_favorites;
        s.rebuild = true;
        drop(s);
        TOGGLE_REVISION.fetch_add(1, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------- quick craft
// The plus button in the recipe viewer sends a recipe's raw pattern/keys/inputs to the server.
// The server resolves concrete items from the player's inventory, checks the grid with the
// engine's own matcher, then moves the ingredients into the open crafting grid.

/// The recipe entry id, read from the host. `RecipeView` keeps its kind and index private, so
/// mirror its two fields (a `String` then a `u32`) to ask `recipe_info` for the entry.
fn recipe_entry(recipe: &RecipeView) -> u32 {
    #[repr(C)]
    struct Mirror {
        kind: String,
        index: u32,
    }
    let mirror: &Mirror = unsafe { std::mem::transmute(recipe) };
    let mut info = abi::RecipeInfo::default();
    let ok = unsafe { (host().recipe_info)(abi::Str::new(&mirror.kind), mirror.index, &mut info) } != 0;
    if ok { info.entry } else { u32::MAX }
}

#[derive(Clone)]
struct QuickCraft {
    shapeless: bool,
    pattern: Vec<String>,
    keys: Vec<(char, String)>,
    inputs: Vec<String>,
    amounts: Vec<u32>,
    output: ItemStack,
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(v)
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.bytes.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes(b.try_into().ok()?))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        if n > 1 << 20 {
            return None;
        }
        let b = self.bytes.get(self.at..self.at + n)?;
        self.at += n;
        String::from_utf8(b.to_vec()).ok()
    }
}

impl QuickCraft {
    fn encode(&self) -> Vec<u8> {
        let mut w = Writer(Vec::new());
        w.u8(1);
        w.u8(self.shapeless as u8);
        w.u32(self.pattern.len() as u32);
        for row in &self.pattern {
            w.str(row);
        }
        w.u32(self.keys.len() as u32);
        for (c, reference) in &self.keys {
            w.u8(*c as u8);
            w.str(reference);
        }
        w.u32(self.inputs.len() as u32);
        for reference in &self.inputs {
            w.str(reference);
        }
        w.u32(self.amounts.len() as u32);
        for n in &self.amounts {
            w.u32(*n);
        }
        w.u32(self.output.item);
        w.u32(self.output.count);
        w.u32(self.output.data);
        w.0
    }

    fn decode(bytes: &[u8]) -> Option<QuickCraft> {
        let mut r = Reader { bytes, at: 0 };
        if r.u8()? != 1 {
            return None;
        }
        let shapeless = r.u8()? != 0;
        let n = r.u32()? as usize;
        if n > 16 {
            return None;
        }
        let mut pattern = Vec::new();
        for _ in 0..n {
            pattern.push(r.str()?);
        }
        let n = r.u32()? as usize;
        if n > 256 {
            return None;
        }
        let mut keys = Vec::new();
        for _ in 0..n {
            let c = r.u8()? as char;
            keys.push((c, r.str()?));
        }
        let n = r.u32()? as usize;
        if n > 256 {
            return None;
        }
        let mut inputs = Vec::new();
        for _ in 0..n {
            inputs.push(r.str()?);
        }
        let n = r.u32()? as usize;
        if n > 256 {
            return None;
        }
        let mut amounts = Vec::new();
        for _ in 0..n {
            amounts.push(r.u32()?);
        }
        let output = ItemStack { item: r.u32()?, count: r.u32()?, data: r.u32()? };
        (r.at == bytes.len() && !output.is_empty()).then_some(QuickCraft { shapeless, pattern, keys, inputs, amounts, output })
    }
}

/// Whether `item` has the tag `tag`; `{m}` matches any material part (oak, iron...).
fn tag_matches(tag: &str, item: u32) -> bool {
    let components = components_of(abi::entry_kind::ITEM, item);
    match tag.split_once('{') {
        None => components.iter().any(|(name, schema)| schema.is_empty() && *name == tag),
        Some((prefix, rest)) => {
            let suffix = rest.split_once('}').map_or("", |(_, s)| s);
            components.iter().any(|(name, schema)| {
                schema.is_empty()
                    && name.len() > prefix.len() + suffix.len()
                    && name.starts_with(prefix)
                    && name.ends_with(suffix)
            })
        }
    }
}

fn item_matches_ref(item: u32, reference: &str) -> bool {
    if let Some(tag) = reference.strip_prefix('#') {
        return tag_matches(tag, item);
    }
    let name = item_name(item);
    name == reference || name.rsplit_once(':').is_some_and(|(_, path)| path == reference)
}

/// The reference with `{m}` filled from the result's own tags, so a stairs recipe picks the
/// stairs of the wood the result is made of instead of any wood.
fn concrete_refs(reference: &str, output: ItemStack) -> Vec<String> {
    let Some((prefix, rest)) = reference.strip_prefix('#').and_then(|r| r.split_once('{')) else {
        return vec![reference.to_string()];
    };
    let suffix = rest.split_once('}').map_or("", |(_, s)| s);
    let mut out = Vec::new();
    for (name, schema) in components_of(abi::entry_kind::ITEM, output.item) {
        if schema.is_empty()
            && name.len() > prefix.len() + suffix.len()
            && name.starts_with(prefix)
            && name.ends_with(suffix)
        {
            out.push(format!("#{}{}{}", prefix, &name[prefix.len()..name.len() - suffix.len()], suffix));
        }
    }
    out
}

struct Requirement {
    cells: Vec<(usize, u32)>,
    reference: String,
    need: u32,
}

/// One requirement per distinct ingredient, with the grid cells and counts it fills.
fn requirements(craft: &QuickCraft, cols: u32, rows: u32) -> Option<Vec<Requirement>> {
    if cols == 0 || rows == 0 {
        return None;
    }
    let cells = (cols * rows) as usize;
    let mut out: Vec<Requirement> = Vec::new();
    if craft.shapeless {
        if craft.inputs.is_empty() || craft.inputs.len() > cells {
            return None;
        }
        for (i, reference) in craft.inputs.iter().enumerate() {
            let amount = craft.amounts.get(i).copied().unwrap_or(1).max(1);
            match out.iter_mut().find(|r| r.reference == *reference) {
                Some(r) => {
                    r.cells.push((i, amount));
                    r.need += amount;
                }
                None => out.push(Requirement { cells: vec![(i, amount)], reference: reference.clone(), need: amount }),
            }
        }
    } else {
        if craft.pattern.is_empty() || craft.pattern.len() > rows as usize {
            return None;
        }
        for (y, row) in craft.pattern.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if x >= cols as usize {
                    return None;
                }
                if c == ' ' {
                    continue;
                }
                let reference = craft.keys.iter().find(|(key, _)| *key == c).map(|(_, r)| r.clone())?;
                let slot = y * cols as usize + x;
                match out.iter_mut().find(|r| r.reference == reference) {
                    Some(r) => {
                        r.cells.push((slot, 1));
                        r.need += 1;
                    }
                    None => out.push(Requirement { cells: vec![(slot, 1)], reference, need: 1 }),
                }
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

fn candidate_items(reference: &str, output: ItemStack, slots: &[(u8, u32, ItemStack)]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    let concrete = concrete_refs(reference, output);
    for (_, _, stack) in slots {
        if !out.contains(&stack.item) && concrete.iter().any(|r| item_matches_ref(stack.item, r)) {
            out.push(stack.item);
        }
    }
    if out.is_empty() {
        for (_, _, stack) in slots {
            if !out.contains(&stack.item) && item_matches_ref(stack.item, reference) {
                out.push(stack.item);
            }
        }
    }
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for (_, _, stack) in slots {
        *counts.entry(stack.item).or_default() += stack.count;
    }
    out.sort_by_key(|item| std::cmp::Reverse(counts.get(item).copied().unwrap_or(0)));
    out.truncate(24);
    out
}

fn build_grid(reqs: &[Requirement], chosen: &[Option<u32>], cols: u32, rows: u32) -> Vec<ItemStack> {
    let mut grid = vec![ItemStack::EMPTY; (cols * rows) as usize];
    for (req, item) in reqs.iter().zip(chosen) {
        let Some(item) = *item else { return Vec::new() };
        for &(slot, count) in &req.cells {
            if slot >= grid.len() {
                return Vec::new();
            }
            grid[slot] = ItemStack { item, count, data: 0 };
        }
    }
    grid
}

#[allow(clippy::too_many_arguments)]
fn solve(
    reqs: &[Requirement],
    candidates: &[Vec<u32>],
    chosen: &mut Vec<Option<u32>>,
    remaining: &mut HashMap<u32, u32>,
    index: usize,
    craft: &QuickCraft,
    cols: u32,
    rows: u32,
    attempts: &mut u32,
) -> Option<Vec<ItemStack>> {
    if index == reqs.len() {
        *attempts += 1;
        if *attempts > 4000 {
            return None;
        }
        let grid = build_grid(reqs, chosen, cols, rows);
        if grid.is_empty() {
            return None;
        }
        let made = recipe_grid_output(&CRAFTING_KIND, &grid, cols, rows);
        return (made.item == craft.output.item && made.count == craft.output.count && made.data == craft.output.data)
            .then_some(grid);
    }
    let need = reqs[index].need;
    for &item in &candidates[index] {
        let have = remaining.get(&item).copied().unwrap_or(0);
        if have < need {
            continue;
        }
        *remaining.get_mut(&item).unwrap() -= need;
        chosen[index] = Some(item);
        if let Some(grid) = solve(reqs, candidates, chosen, remaining, index + 1, craft, cols, rows, attempts) {
            return Some(grid);
        }
        chosen[index] = None;
        *remaining.get_mut(&item).unwrap() += need;
    }
    None
}

fn fill_craft(ctx: &MenuCtx, craft: &QuickCraft) -> bool {
    let Some(grid_group) = ctx.find_group("grid") else { return false };
    let (cols, rows) = (grid_group.cols(), grid_group.rows());
    if cols == 0 || rows == 0 {
        return false;
    }
    if (0..grid_group.len()).any(|i| !grid_group.get(i).is_empty()) {
        return false;
    }
    let Some(reqs) = requirements(craft, cols, rows) else { return false };

    let mut slots: Vec<(u8, u32, ItemStack)> = Vec::new();
    for (group, name) in [INVENTORY_GROUP, HOTBAR_GROUP].into_iter().enumerate() {
        if let Some(g) = ctx.find_group(name) {
            for i in 0..g.len() {
                let stack = g.get(i);
                if !stack.is_empty() {
                    slots.push((group as u8, i, stack));
                }
            }
        }
    }
    let mut remaining: HashMap<u32, u32> = HashMap::new();
    for (_, _, stack) in &slots {
        *remaining.entry(stack.item).or_default() += stack.count;
    }
    let candidates: Vec<Vec<u32>> = reqs.iter().map(|r| candidate_items(&r.reference, craft.output, &slots)).collect();
    if candidates.iter().any(|c| c.is_empty()) {
        return false;
    }
    let mut chosen = vec![None; reqs.len()];
    let mut attempts = 0;
    let Some(grid) = solve(&reqs, &candidates, &mut chosen, &mut remaining, 0, craft, cols, rows, &mut attempts) else {
        return false;
    };

    // Work out what to take before changing anything.
    let mut need: HashMap<u32, u32> = HashMap::new();
    for (req, item) in reqs.iter().zip(&chosen) {
        if let Some(item) = item {
            *need.entry(*item).or_default() += req.need;
        }
    }
    let mut plan: Vec<(u8, u32, ItemStack)> = Vec::new();
    for (group, i, stack) in &slots {
        let Some(left) = need.get_mut(&stack.item) else { continue };
        if *left == 0 {
            continue;
        }
        let take = (*left).min(stack.count);
        *left -= take;
        if take == 0 {
            continue;
        }
        let rest = stack.count - take;
        plan.push((*group, *i, if rest == 0 { ItemStack::EMPTY } else { ItemStack { count: rest, ..*stack } }));
    }
    if need.values().any(|&n| n != 0) {
        return false;
    }
    for (group, i, stack) in plan {
        if let Some(g) = ctx.find_group([INVENTORY_GROUP, HOTBAR_GROUP][group as usize]) {
            g.set(i, stack);
        }
    }
    for (i, stack) in grid.iter().enumerate() {
        if !stack.is_empty() {
            grid_group.set(i as u32, *stack);
        }
    }
    if let Some(result) = ctx.find_group("result") {
        result.set(0, craft.output);
    }
    true
}

fn quick_craft_tick(ctx: &MenuCtx) {
    let player = ctx.player();
    let craft = {
        let mut pending = PENDING_CRAFT.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|(p, at, _)| *p != player || at.elapsed().as_secs_f32() < 2.0);
        pending.iter().find(|(p, _, _)| *p == player).map(|(_, _, c)| c.clone())
    };
    let Some(craft) = craft else { return };
    {
        let mut pending = PENDING_CRAFT.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|(p, _, _)| *p != player);
    }
    fill_craft(ctx, &craft);
}

fn open_grid(menu: Option<abi::OpenMenu>) -> (u32, u32) {
    match menu {
        Some(m) if m.kind == CRAFTING_MENU.id() => (3, 3),
        Some(m) if m.kind == INVENTORY_MENU.id() => (2, 2),
        _ => (0, 0),
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ':' | '_' | '-' => ' ',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

fn filtered_items(search: &str) -> Vec<u32> {
    let search = search.trim();
    let re = if search.is_empty() {
        None
    } else {
        regex_lite::Regex::new(search).ok()
    };
    let literal = search.to_lowercase();
    let mut out: Vec<u32> = (1..entry_count(abi::entry_kind::ITEM))
        .filter(|&id| {
            let name = item_name(id);
            if name.is_empty() {
                return false;
            }
            if search.is_empty() {
                return true;
            }
            let norm = normalize(name);
            if let Some(re) = &re {
                re.is_match(&norm) || re.is_match(name)
            } else {
                norm.contains(&literal) || name.to_lowercase().contains(&literal)
            }
        })
        .collect();
    out.sort_by_key(|&id| item_name(id));
    out
}

fn make_item_button(ui: &mut Ui, id: u32) {
    ui.button_content(
        S::default().class("tab").size(SLOT, SLOT).pad(1.0),
        id,
        move |c, item_id| {
            let mut st = state();
            let mut changed = false;
            if st.selecting {
                if let Some(pos) = st.favorites.iter().position(|&x| x == item_id) {
                    st.favorites.remove(pos);
                } else {
                    st.favorites.push(item_id);
                }
                st.rebuild = true;
                changed = true;
            } else {
                st.pending_recipe = item_id;
            }
            if changed {
                FAV_REVISION.fetch_add(1, Ordering::SeqCst);
                let rd = c.client().run_dir();
                let dir = format!("{}/config", rd);
                let _ = std::fs::create_dir_all(&dir);
                let path = format!("{}/jei_sidebar_favorites.txt", dir);
                let contents: String = st.favorites.iter().map(|id| id.to_string()).collect::<Vec<_>>().join("\n");
                drop(st);
                let _ = std::fs::write(&path, contents);
            }
        },
        |ui| ui.item(id, 1, S::default().size(16.0, 16.0)),
    );
}

fn init(r: &mut Registry) {
    r.menu(&DUMMY).screen(|_| {});

    // The recipe viewer's display for crafting: the usual inputs, arrow and outputs, plus a
    // button that moves the ingredients from the player's inventory into the open grid.
    r.edit(&CRAFTING_KIND).layout(|ui, recipe| {
        let entry = recipe_entry(recipe);
        let pattern = comps::recipe::PATTERN.on(abi::entry_kind::RECIPE, entry).unwrap_or_default();
        let keys = comps::recipe::KEYS.on(abi::entry_kind::RECIPE, entry).unwrap_or_default();
        let inputs = comps::recipe::INPUTS.on(abi::entry_kind::RECIPE, entry).unwrap_or_default();
        let amounts = comps::recipe::AMOUNTS.on(abi::entry_kind::RECIPE, entry).unwrap_or_default();
        let output = recipe.outputs().first().map(|(stack, _)| *stack).unwrap_or(ItemStack::EMPTY);
        let shapeless = pattern.is_empty();
        let (cols, rows) = open_grid(ui.view().client().menu());
        let fits = cols > 0
            && !output.is_empty()
            && if shapeless {
                !inputs.is_empty() && inputs.len() <= (cols * rows) as usize
            } else {
                !pattern.is_empty()
                    && pattern.len() <= rows as usize
                    && pattern.iter().all(|row| row.chars().count() <= cols as usize)
            };
        let craft = QuickCraft {
            shapeless,
            pattern,
            keys: keys.iter().map(|key| (key.name.chars().next().unwrap_or(' '), key.item.clone())).collect(),
            inputs,
            amounts,
            output,
        };
        let shapeless = craft.shapeless;
        let input_count = craft.inputs.len();
        ui.column(S::column().gap(2.0), |ui| {
            ui.row(S::row().gap(6.0), |ui| {
                ui.recipe_inputs();
                ui.row(S::row().gap(1.0).center(), |ui| {
                    ui.recipe_arrow();
                    if shapeless && input_count > 1 {
                        ui.image("core:gui/icon_shapeless", 9.0, 9.0);
                    }
                });
                ui.recipe_outputs();
                let send = craft.clone();
                ui.button(
                    "jei_sidebar.plus",
                    S::default().size(18.0, 18.0).pad(0.0).disabled(!fits).tooltip("jei_sidebar.quick_craft"),
                    move |c| {
                        if open_grid(c.client().menu()).0 != 0 {
                            c.client().send(&QUICK_CRAFT, &send.encode());
                            c.client().menu_close();
                        }
                    },
                );
            });
        });
    });

    r.packet(&QUICK_CRAFT, |p, bytes| {
        if let Some(craft) = QuickCraft::decode(&bytes) {
            let mut pending = PENDING_CRAFT.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|(_, at, _)| at.elapsed().as_secs_f32() < 2.0);
            pending.push((p.player(), Instant::now(), craft));
        }
    });
    r.edit(&CRAFTING_MENU).on_tick(quick_craft_tick);
    r.edit(&INVENTORY_MENU).on_tick(quick_craft_tick);

    // Bottom-right HUD toggle button for favorites visibility
    r.hud_element(&FAV_TOGGLE_ELEMENT, hud_anchor::BOTTOM_RIGHT, 0, |ui| {
        let last_screen = state().last_screen.clone();
        if last_screen != "core:menu" {
            return;
        }
        let favorites = state().favorites.clone();
        if favorites.is_empty() {
            return;
        }
        let show = state().show_favorites;
        ui.button(
            if show { "jei_sidebar.hide_icon" } else { "jei_sidebar.show_icon" },
            S::default()
                .size(SLOT, SLOT)
                .pad(1.0)
                .tooltip(if show { "jei_sidebar.hide_fav" } else { "jei_sidebar.show_fav" }),
            move |_| {
                let mut st = state();
                st.show_favorites = !st.show_favorites;
                st.rebuild = true;
                TOGGLE_REVISION.fetch_add(1, Ordering::SeqCst);
            },
        );
    })
    .rebuild_when(|_| (FAV_REVISION.load(Ordering::SeqCst) + TOGGLE_REVISION.load(Ordering::SeqCst)) as u64);

    r.screen_part(&screens::MENUS, "side", 0, |ui| {
        let (search, page, favorites, selecting, fav_page, show_favorites) = {
            let st = state();
            (st.search.clone(), st.page, st.favorites.clone(), st.selecting, st.fav_page, st.show_favorites)
        };
        let items = filtered_items(&search);
        let total_pages = if items.is_empty() {
            1
        } else {
            (items.len() + PER_PAGE - 1) / PER_PAGE
        };
        let page = page.min(total_pages - 1);
        {
            let mut st = state();
            if st.page != page {
                st.page = page;
            }
        }
        let start = page * PER_PAGE;

        // Favorites pagination
        let total_fav_pages = if favorites.is_empty() {
            1
        } else {
            (favorites.len() + PER_PAGE - 1) / PER_PAGE
        };
        let fav_page = fav_page.min(total_fav_pages - 1);
        {
            let mut st = state();
            if st.fav_page != fav_page {
                st.fav_page = fav_page;
            }
        }
        let fav_start = fav_page * PER_PAGE;

        let show_fav_col = !favorites.is_empty() && show_favorites;

        let mut panel_style = S::row();
        panel_style.align = abi::ui_align::START;
        let panel_style = panel_style
            .pad(PAD)
            .gap(GAP)
            .w(if show_fav_col { WIDE_PANEL_W } else { PANEL_W });
        ui.panel("sidebar", panel_style, |ui| {
            // Search grid column.
            ui.column(S::column().gap(GAP).w(GRID_W), |ui| {
                // Search
                ui.input(&search, 64, S::column().class("input").w(GRID_W).h(18.0), move |_, text| {
                    let mut st = state();
                    st.search = text.to_string();
                    st.page = 0;
                    st.rebuild = true;
                });

                // Main grid
                ui.column(S::column().gap(GAP).w(GRID_W), |ui| {
                    for row in 0..ROWS {
                        ui.row(S::row().gap(GAP).w(GRID_W).h(SLOT), |ui| {
                            for col in 0..COLUMNS {
                                let idx = start + row * COLUMNS + col;
                                match items.get(idx) {
                                    Some(&id) => make_item_button(ui, id),
                                    None => {
                                        ui.column(S::default().size(SLOT, SLOT), |_| {});
                                    }
                                }
                            }
                        });
                    }
                });

                // Pagination + star button
                ui.row(S::row().gap(GAP).w(GRID_W).h(SLOT), |ui| {
                    ui.button(
                        "jei_sidebar.prev",
                        S::default().size(SLOT, SLOT).pad(1.0).disabled(page == 0),
                        move |_| {
                            let mut st = state();
                            if st.page > 0 {
                                st.page -= 1;
                                st.rebuild = true;
                            }
                        },
                    );
                    ui.button(
                        "\u{2605}",
                        S::default().size(SLOT, SLOT).pad(1.0).selected(selecting),
                        move |_| {
                            let mut st = state();
                            st.selecting = !st.selecting;
                            st.rebuild = true;
                        },
                    );
                    ui.spacer();
                    ui.text_styled(
                        S::default().no_wrap().min_w(24.0),
                        "jei_sidebar.page",
                        &[Arg::Int((page + 1) as i64), Arg::Int(total_pages as i64)],
                    );
                    ui.spacer();
                    ui.button(
                        "jei_sidebar.next",
                        S::default().size(SLOT, SLOT).pad(1.0).disabled(page + 1 >= total_pages),
                        move |_| {
                            let mut st = state();
                            if st.page + 1 < total_pages {
                                st.page += 1;
                                st.rebuild = true;
                            }
                        },
                    );
                });
            });

            // Favorites grid (shown when non-empty and visible), right column
            if show_fav_col {
                ui.column(S::column().gap(GAP).w(GRID_W), |ui| {
                    ui.text("jei_sidebar.favorites");
                    ui.column(S::column().gap(GAP).w(GRID_W), |ui| {
                        for row in 0..ROWS {
                            ui.row(S::row().gap(GAP).w(GRID_W).h(SLOT), |ui| {
                                for col in 0..COLUMNS {
                                    let idx = fav_start + row * COLUMNS + col;
                                    match favorites.get(idx) {
                                        Some(&id) => make_item_button(ui, id),
                                        None => {
                                            ui.column(S::default().size(SLOT, SLOT), |_| {});
                                        }
                                    }
                                }
                            });
                        }
                    });
                    // Favorites pagination
                    ui.row(S::row().gap(GAP).w(GRID_W).h(SLOT), |ui| {
                        ui.button(
                            "jei_sidebar.prev",
                            S::default().size(SLOT, SLOT).pad(1.0).disabled(fav_page == 0),
                            move |_| {
                                let mut st = state();
                                if st.fav_page > 0 {
                                    st.fav_page -= 1;
                                    st.rebuild = true;
                                }
                            },
                        );
                        ui.spacer();
                        ui.text_styled(
                            S::default().no_wrap(),
                            "jei_sidebar.page",
                            &[Arg::Int((fav_page + 1) as i64), Arg::Int(total_fav_pages as i64)],
                        );
                        ui.spacer();
                        ui.button(
                            "jei_sidebar.next",
                            S::default().size(SLOT, SLOT).pad(1.0).disabled(fav_page + 1 >= total_fav_pages),
                            move |_| {
                                let mut st = state();
                                if st.fav_page + 1 < total_fav_pages {
                                    st.fav_page += 1;
                                    st.rebuild = true;
                                }
                            },
                        );
                    });
                });
            }
        });
    });

    r.client_frame(move |c| {
        let screen = c.screen();
        // Load favorites from disk on first frame, and rebuild the HUD whenever the top
        // screen changes so the corner button appears and disappears with the menu.
        {
            let mut st = state();
            if !st.loaded {
                st.loaded = true;
                let rd = c.run_dir();
                let path = format!("{}/config/jei_sidebar_favorites.txt", rd);
                if let Ok(contents) = std::fs::read_to_string(&path) {
                    st.favorites = contents.lines().filter_map(|l| l.trim().parse().ok()).collect();
                }
                FAV_REVISION.fetch_add(1, Ordering::SeqCst);
            }
            if st.last_screen != screen {
                st.last_screen = screen.clone();
                FAV_REVISION.fetch_add(1, Ordering::SeqCst);
            }
        }
        // HUD widgets take no mouse input, so the corner button is hit-tested by hand.
        #[cfg(target_os = "windows")]
        corner_button_click(c, &screen);
        let (do_rebuild, pending) = {
            let mut st = state();
            let r = (st.rebuild, st.pending_recipe);
            st.rebuild = false;
            st.pending_recipe = 0;
            r
        };
        if do_rebuild && !screen.is_empty() {
            c.menu_open(&DUMMY);
            c.menu_close();
        }
        if pending != 0 && !screen.is_empty() {
            c.input_action(&keys::RECIPES, 0.0);
        }
    });
}
