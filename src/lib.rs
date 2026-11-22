// Jei-style item browser sidebar for Weft
// Shows a grid of items on the right side of inventory screens
// with pagination and search functionality. Clicking an item opens
// Weft's built-in recipe viewer. Star button toggles favorite selection.

use weft_sdk::*;

use std::sync::Mutex;

declare_mod!(deps: ["base"], init: init);

const COLUMNS: usize = 5;
const ROWS: usize = 6;
const PER_PAGE: usize = COLUMNS * ROWS;
const SLOT: f32 = 20.0;
const GAP: f32 = 2.0;
const PAD: f32 = 4.0;
const GRID_W: f32 = COLUMNS as f32 * SLOT + (COLUMNS as f32 - 1.0) * GAP;
const PANEL_W: f32 = GRID_W + PAD * 2.0;
const WIDE_PANEL_W: f32 = PANEL_W * 2.0 + GAP;

static DUMMY: Menu = Menu::named("jei_sidebar_rebuild");

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
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
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
    let needle = normalize(search);
    let needle = needle.trim();
    let mut out: Vec<u32> = (1..entry_count(abi::entry_kind::ITEM))
        .filter(|&id| {
            let name = item_name(id);
            if name.is_empty() {
                return false;
            }
            if needle.is_empty() {
                return true;
            }
            normalize(name).contains(needle)
        })
        .collect();
    out.sort_by_key(|&id| item_name(id));
    out
}

fn make_item_button(ui: &mut Ui, id: u32) {
    ui.button_content(
        S::default().class("slot").size(SLOT, SLOT).pad(1.0),
        id,
        move |_, item_id| {
            let mut st = state();
            if st.selecting {
                if let Some(pos) = st.favorites.iter().position(|&x| x == item_id) {
                    st.favorites.remove(pos);
                } else {
                    st.favorites.push(item_id);
                }
                st.rebuild = true;
            } else {
                st.pending_recipe = item_id;
            }
        },
        |ui| ui.item(id, 1, S::default().size(16.0, 16.0)),
    );
}

fn init(r: &mut Registry) {
    r.menu(&DUMMY).screen(|_| {});

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
        ui.panel("sidebar", S::column().pad(PAD).gap(GAP).w(if show_fav_col { WIDE_PANEL_W } else { PANEL_W }), |ui| {

            ui.row(S::row().gap(GAP), |ui| {
                // Left column: search + main grid
                ui.column(S::column().gap(GAP).w(PANEL_W), |ui| {
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
                        ui.text_args(
                            "jei_sidebar.page",
                            &[Arg::Int((page + 1) as i64), Arg::Int(total_pages as i64)],
                        );
                        if !favorites.is_empty() {
                            ui.button(
                                if show_favorites { "\u{2715}" } else { "\u{25CB}" },
                                S::default().size(SLOT, SLOT).pad(1.0),
                                move |_| {
                                    let mut st = state();
                                    st.show_favorites = !st.show_favorites;
                                    st.rebuild = true;
                                },
                            );
                        }
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

                // Right column: favorites (shown when non-empty and visible)
                if show_fav_col {
                    ui.column(S::column().gap(GAP).w(PANEL_W), |ui| {
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
                            ui.text_args(
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
    });

    r.client_frame(move |c| {
        let (do_rebuild, pending) = {
            let mut st = state();
            let r = (st.rebuild, st.pending_recipe);
            st.rebuild = false;
            st.pending_recipe = 0;
            r
        };
        if do_rebuild && !c.screen().is_empty() {
            c.menu_open(&DUMMY);
            c.menu_close();
        }
        if pending != 0 && !c.screen().is_empty() {
            c.input_action(&keys::RECIPES, 0.0);
        }
    });
}
