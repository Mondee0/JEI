// Jei-style item browser sidebar for Weft
// Shows a grid of items on the right side of inventory screens
// with pagination and search functionality. Clicking an item opens
// Weft's built-in recipe viewer. Star button toggles favorite selection.

use weft_sdk::*;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
        S::default().class("slot").size(SLOT, SLOT).pad(1.0),
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
                                        ui.panel("slot", S::default().size(SLOT, SLOT), |_| {});
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
                                            ui.panel("slot", S::default().size(SLOT, SLOT), |_| {});
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
