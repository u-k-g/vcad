import { describe, expect, it } from "vitest";
import { isInputEvent, isInputFocused } from "@vcad/core";

describe("text input keybinding isolation", () => {
  it("recognizes native and ARIA text controls", () => {
    const textarea = document.createElement("textarea");
    const textbox = document.createElement("div");
    textbox.setAttribute("role", "textbox");

    expect(isInputFocused(textarea)).toBe(true);
    expect(isInputFocused(textbox)).toBe(true);
    expect(isInputFocused(document.createElement("div"))).toBe(false);
  });

  it("recognizes descendants of contenteditable controls", () => {
    const editor = document.createElement("div");
    editor.contentEditable = "true";
    const nested = document.createElement("span");
    editor.append(nested);

    expect(isInputFocused(nested)).toBe(true);
  });

  it("keeps keyboard events local to the focused editor", () => {
    const textarea = document.createElement("textarea");
    document.body.append(textarea);
    textarea.focus();

    const event = new KeyboardEvent("keydown", {
      key: "a",
      metaKey: true,
      bubbles: true,
      composed: true,
    });
    textarea.dispatchEvent(event);

    expect(isInputEvent(event)).toBe(true);
    textarea.remove();
  });
});
