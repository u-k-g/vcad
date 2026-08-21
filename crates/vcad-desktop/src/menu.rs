//! Native application menu bar.
//!
//! Mirrors the in-window Menubar (see `packages/app/src/components/Header.tsx`)
//! so macOS gets its system menu at the top of the screen. Each item's id
//! matches a command registry id on the JS side; on click we emit a
//! `menu-command` event and the webview dispatches it through the registry.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{
    menu::{MenuBuilder, MenuItem, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder},
    AppHandle, Emitter, Manager, Runtime,
};

pub const MENU_EVENT: &str = "menu-command";

/// App-managed state that holds the menu item handles so we can update
/// `enabled` from the webview at runtime. Wrapped in a generic Mutex keyed
/// on item id — reads are rare (only during JS-driven sync).
pub struct MenuState<R: Runtime> {
    items: Mutex<HashMap<String, MenuItem<R>>>,
    enabled: Mutex<HashMap<String, bool>>,
    text_input_focused: Mutex<bool>,
}

impl<R: Runtime> MenuState<R> {
    fn new() -> Self {
        Self {
            items: Mutex::new(HashMap::new()),
            enabled: Mutex::new(HashMap::new()),
            text_input_focused: Mutex::new(false),
        }
    }

    fn insert(&self, id: &str, item: MenuItem<R>) {
        if let Ok(enabled) = self.enabled.lock() {
            if let Some(enabled) = enabled.get(id) {
                let _ = item.set_enabled(*enabled);
            }
        }
        if let Ok(mut map) = self.items.lock() {
            map.insert(id.to_string(), item);
        }
    }

    fn clear_items(&self) {
        if let Ok(mut map) = self.items.lock() {
            map.clear();
        }
    }

    /// Returns true only when the native Edit menu needs rebuilding.
    fn set_text_input_focused(&self, focused: bool) -> bool {
        let Ok(mut current) = self.text_input_focused.lock() else {
            return false;
        };
        if *current == focused {
            return false;
        }
        *current = focused;
        true
    }

    /// Set `enabled` on the item with `id`. Silently ignores unknown ids —
    /// lets the JS side send broader maps without tight coupling.
    pub fn set_enabled(&self, id: &str, enabled: bool) {
        if let Ok(mut values) = self.enabled.lock() {
            values.insert(id.to_string(), enabled);
        }
        if let Ok(map) = self.items.lock() {
            if let Some(item) = map.get(id) {
                let _ = item.set_enabled(enabled);
            }
        }
    }
}

#[derive(Serialize, Clone)]
struct MenuCommand<'a> {
    id: &'a str,
}

/// Item spec: `(id, label_key, accelerator)`. `label_key` is an i18n key
/// resolved via `vcad_i18n::t()` at menu construction time. `accelerator`
/// follows Tauri's `CmdOrCtrl+...` grammar.
type Item<'a> = (&'a str, &'a str, Option<&'a str>);

/// One group of items in a submenu, rendered with a trailing separator.
type Group<'a> = &'a [Item<'a>];

const FILE_GROUPS: &[Group] = &[
    &[
        ("new-document", "menu.file.new", Some("CmdOrCtrl+N")),
        ("open", "menu.file.open", Some("CmdOrCtrl+O")),
        (
            "open-recent",
            "desktop.menu.open_recent",
            Some("CmdOrCtrl+Shift+O"),
        ),
        ("open-cloud", "desktop.menu.open_cloud", None),
    ],
    &[("save", "menu.file.save", Some("CmdOrCtrl+S"))],
];

// These accelerators are installed only while the viewport owns focus. When
// an HTML editor is focused, the whole submenu is replaced with Cocoa's
// predefined responder actions (copy:, paste:, selectAll:, undo:, redo:).
const EDIT_GROUPS: &[Group] = &[
    &[
        ("undo", "menu.edit.undo", Some("CmdOrCtrl+Z")),
        ("redo", "menu.edit.redo", Some("CmdOrCtrl+Shift+Z")),
    ],
    &[
        ("copy", "cmd.copy.label", Some("CmdOrCtrl+C")),
        ("paste", "cmd.paste.label", Some("CmdOrCtrl+V")),
        ("duplicate", "menu.edit.duplicate", Some("CmdOrCtrl+D")),
        ("delete", "menu.edit.delete", None),
    ],
    &[
        ("select-all", "menu.edit.select_all", Some("CmdOrCtrl+A")),
        ("deselect", "menu.edit.deselect", None),
    ],
];

const VIEW_GROUPS: &[Group] = &[
    &[
        ("toggle-sidebar", "menu.view.toggle_sidebar", None),
        ("toggle-chat", "menu.view.toggle_chat", None),
        ("toggle-status-bar", "desktop.menu.toggle_status_bar", None),
        ("toggle-devtools", "cmd.toggle_devtools.label", None),
    ],
    &[
        ("camera-isometric", "desktop.menu.camera_iso", None),
        ("camera-top", "desktop.menu.camera_top", None),
        ("camera-front", "desktop.menu.camera_front", None),
        ("camera-right", "desktop.menu.camera_right", None),
        ("camera-fit", "desktop.menu.camera_fit", None),
    ],
    &[
        ("toggle-wireframe", "cmd.toggle_wireframe.label", None),
        ("toggle-grid-snap", "desktop.menu.toggle_grid_snap", None),
    ],
    &[("cycle-theme", "cmd.cycle_theme.label", None)],
];

const TOOLS_GROUPS: &[Group] = &[
    &[("command-palette", "menu.tools.palette", None)],
    &[("new-sketch", "menu.tools.sketch", None)],
    &[
        ("open-slicer", "desktop.menu.slicer", None),
        ("open-cam", "desktop.menu.cam", None),
    ],
];

const HELP_GROUPS: &[Group] = &[
    &[
        ("about", "menu.help.about", None),
        ("check-for-updates", "desktop.menu.check_for_updates", None),
        ("whats-new", "desktop.menu.whats_new", None),
    ],
    &[
        ("open-docs", "desktop.menu.documentation", None),
        ("open-github", "menu.help.github", None),
        ("open-discord", "menu.help.discord", None),
    ],
];

fn build_submenu<R: Runtime>(
    app: &AppHandle<R>,
    state: &MenuState<R>,
    label_key: &str,
    groups: &[Group],
) -> tauri::Result<tauri::menu::Submenu<R>> {
    let mut sub = SubmenuBuilder::new(app, vcad_i18n::t(label_key));
    for (gi, group) in groups.iter().enumerate() {
        for (id, label_key, accel) in group.iter() {
            let mut b = MenuItemBuilder::with_id(*id, vcad_i18n::t(label_key));
            if let Some(a) = accel {
                b = b.accelerator(*a);
            }
            let item = b.build(app)?;
            state.insert(id, item.clone());
            sub = sub.item(&item);
        }
        if gi + 1 < groups.len() {
            sub = sub.separator();
        }
    }
    Ok(sub.build()?)
}

/// Cocoa text controls implement editing through the responder chain, not by
/// receiving the custom menu events above. These predefined items carry the
/// standard selectors and key equivalents required by WKWebView textareas.
fn build_text_edit_submenu<R: Runtime>(
    app: &AppHandle<R>,
) -> tauri::Result<tauri::menu::Submenu<R>> {
    let undo_label = vcad_i18n::t("menu.edit.undo");
    let redo_label = vcad_i18n::t("menu.edit.redo");
    let copy_label = vcad_i18n::t("cmd.copy.label");
    let paste_label = vcad_i18n::t("cmd.paste.label");
    let select_all_label = vcad_i18n::t("menu.edit.select_all");

    let undo = PredefinedMenuItem::undo(app, Some(&undo_label))?;
    let redo = PredefinedMenuItem::redo(app, Some(&redo_label))?;
    let cut = PredefinedMenuItem::cut(app, None)?;
    let copy = PredefinedMenuItem::copy(app, Some(&copy_label))?;
    let paste = PredefinedMenuItem::paste(app, Some(&paste_label))?;
    let select_all = PredefinedMenuItem::select_all(app, Some(&select_all_label))?;

    Ok(SubmenuBuilder::new(app, vcad_i18n::t("menu.edit"))
        .items(&[&undo, &redo])
        .separator()
        .items(&[&cut, &copy, &paste])
        .separator()
        .item(&select_all)
        .build()?)
}

fn replace_menu<R: Runtime>(
    app: &AppHandle<R>,
    state: &MenuState<R>,
    text_input_focused: bool,
) -> tauri::Result<()> {
    state.clear_items();

    // App menu (macOS standard first menu). On other platforms Tauri still
    // accepts these predefined items but they're less load-bearing.
    let app_name = app
        .config()
        .product_name
        .clone()
        .unwrap_or_else(|| "vcad".to_string());
    let app_menu = SubmenuBuilder::new(app, &app_name)
        .item(&PredefinedMenuItem::about(app, None, None)?)
        .separator()
        .item(&PredefinedMenuItem::services(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::hide(app, None)?)
        .item(&PredefinedMenuItem::hide_others(app, None)?)
        .item(&PredefinedMenuItem::show_all(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, None)?)
        .build()?;

    let file = build_submenu(app, state, "menu.file", FILE_GROUPS)?;
    let edit = if text_input_focused {
        build_text_edit_submenu(app)?
    } else {
        build_submenu(app, state, "menu.edit", EDIT_GROUPS)?
    };
    let view = build_submenu(app, state, "menu.view", VIEW_GROUPS)?;
    let tools = build_submenu(app, state, "menu.tools", TOOLS_GROUPS)?;
    let help = build_submenu(app, state, "menu.help", HELP_GROUPS)?;

    // Window submenu (minimize / zoom / close) — expected by macOS users.
    let window = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, None)?)
        .build()?;

    let menu = MenuBuilder::new(app)
        .items(&[&app_menu, &file, &edit, &view, &tools, &window, &help])
        .build()?;

    app.set_menu(menu)?;
    Ok(())
}

/// Build and attach the native menu to `app`. On macOS this becomes the
/// system menu bar at the top of the screen; on Windows/Linux it's attached
/// to the main window. Registers `MenuState<R>` as a Tauri-managed resource
/// so its editing mode and command enabled state can be updated later.
pub fn install<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let state = MenuState::<R>::new();
    replace_menu(app, &state, false)?;
    app.manage(state);
    Ok(())
}

/// Invoke command — apply `enabled` state to the listed ids. JS dispatches
/// this whenever the command registry's enabled() results change, keeping
/// the native menu's greying/accelerators in sync with app state.
#[tauri::command]
pub fn set_menu_enabled<R: Runtime>(app: AppHandle<R>, items: HashMap<String, bool>) {
    if let Some(state) = app.try_state::<MenuState<R>>() {
        for (id, enabled) in items {
            state.set_enabled(&id, enabled);
        }
    }
}

/// Swap between VCAD's CAD Edit commands and the platform's native text
/// responder actions. A custom `Cmd+V` menu item can never paste into a
/// WKWebView textarea; the predefined Cocoa `paste:` selector is required.
#[tauri::command]
pub fn set_text_input_focused<R: Runtime>(app: AppHandle<R>, focused: bool) -> Result<(), String> {
    let Some(state) = app.try_state::<MenuState<R>>() else {
        return Ok(());
    };
    if !state.set_text_input_focused(focused) {
        return Ok(());
    }
    replace_menu(&app, &state, focused).map_err(|error| error.to_string())
}

/// Handle a click on a managed menu item — emit the event for the webview.
/// Called from `main.rs` inside `on_menu_event`.
pub fn handle_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    // PredefinedMenuItem ids (about/quit/etc.) never reach here in a form
    // our JS listens for — skip emitting so we don't round-trip them.
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.emit(MENU_EVENT, MenuCommand { id });
    }
}
