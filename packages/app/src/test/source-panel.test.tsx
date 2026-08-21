import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { useDocumentStore, useUiStore } from "@vcad/core";
import { SourcePanel } from "@/components/ChatSidebar";

const originalDocumentState = useDocumentStore.getState();
const originalUiState = useUiStore.getState();

afterEach(() => {
  cleanup();
  useDocumentStore.setState(originalDocumentState, true);
  useUiStore.setState(originalUiState, true);
});

describe("SourcePanel", () => {
  it("shows preserved Loon source immediately even during an active model", () => {
    const source = "[let shape [cube 20.0 20.0 20.0]]\n[root shape \"default\"]\n";
    useDocumentStore.setState({ loonSource: source });
    useUiStore.setState({ isDraggingGizmo: false });

    render(<SourcePanel />);

    expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe(source);
    expect(screen.getByText(/Synced with document/)).toBeTruthy();
  });
});
