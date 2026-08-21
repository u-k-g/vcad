/**
 * Native (Tauri) menu → command registry bridge.
 *
 * The Rust side (`crates/vcad-desktop/src/menu.rs`) builds the system menu
 * and emits a `menu-command` event with the clicked item's id. We resolve
 * the id against the active command registry and run the action — so any
 * command that already works from the in-window Menubar works from the
 * native menu bar for free.
 */

import { useEffect } from "react";
import { isTauri, invoke } from "@/lib/tauri";
import { isInputFocused, type Command } from "@vcad/core";

const MENU_EVENT = "menu-command";

interface MenuCommandPayload {
  id: string;
}

export function useNativeMenu(commands: Command[]) {
  // Cocoa routes standard editing shortcuts through native responder actions
  // (`paste:`, `selectAll:`, etc.), not through a webview keydown event. Tell
  // the desktop host when an editor owns focus so it can expose the native
  // Edit menu; switch back to CAD accelerators when focus leaves the editor.
  useEffect(() => {
    if (!isTauri()) return;

    let lastFocused: boolean | undefined;
    const syncFocus = () => {
      const focused = isInputFocused(document.activeElement);
      if (focused === lastFocused) return;
      lastFocused = focused;
      void invoke("set_text_input_focused", { focused }).catch(() => {
        // Retry on the next focus transition if the native menu is still
        // starting up or was rebuilt during a hot reload.
        lastFocused = undefined;
      });
    };
    const syncAfterFocusOut = () => queueMicrotask(syncFocus);

    document.addEventListener("focusin", syncFocus, true);
    document.addEventListener("focusout", syncAfterFocusOut, true);
    syncFocus();
    return () => {
      document.removeEventListener("focusin", syncFocus, true);
      document.removeEventListener("focusout", syncAfterFocusOut, true);
    };
  }, []);

  useEffect(() => {
    if (!isTauri()) return;
    let dispose: (() => void) | undefined;
    let active = true;
    (async () => {
      const { listen } = await import("@tauri-apps/api/event");
      if (!active) return;
      const unlisten = await listen<MenuCommandPayload>(MENU_EVENT, (event) => {
        const id = event.payload?.id;
        if (!id) return;
        // Menu items that aren't registry commands — handled inline so we
        // don't have to pollute the shared registry with desktop-only entries.
        if (id === "open-recent") {
          window.dispatchEvent(new CustomEvent("vcad:open-recent-files"));
          return;
        }
        if (id === "check-for-updates") {
          void import("@/lib/native-updater").then((m) =>
            m.checkForUpdates({ announceIfUpToDate: true }),
          );
          return;
        }
        const cmd = commands.find((c) => c.id === id);
        if (!cmd) {
          console.warn(`[menu] no command registered for id "${id}"`);
          return;
        }
        if (cmd.enabled && !cmd.enabled()) return;
        cmd.action();
      });
      dispose = unlisten;
    })();
    return () => {
      active = false;
      dispose?.();
    };
  }, [commands]);

  // Sync enabled-state from the registry into the native menu so items grey
  // out correctly (Undo when no history, Paste when clipboard empty, etc.).
  // Runs on every render where the parent subscribed to state that changes
  // command.enabled() outputs — Header.tsx already does this.
  useEffect(() => {
    if (!isTauri()) return;
    const items: Record<string, boolean> = {};
    for (const cmd of commands) {
      if (!cmd.enabled) continue;
      items[cmd.id] = cmd.enabled();
    }
    if (Object.keys(items).length === 0) return;
    void invoke("set_menu_enabled", { items }).catch(() => {
      // Menu sync is cosmetic — swallow errors so we don't spam the console
      // during the brief window before the native menu is installed.
    });
  });
}
