/** Native Codex subscription bridge for VCAD desktop. */

import { Channel } from "@tauri-apps/api/core";
import { invoke, isTauri } from "@/lib/tauri";
import type { ChatRequestMessage } from "@/lib/chat-api";
import {
  buildCodexRequest,
  CodexSseAccumulator,
  type CodexRequestOptions,
  type CodexStreamCallbacks,
} from "@/lib/codex-protocol";

export interface CodexAuthStatus {
  available: boolean;
  loggedIn: boolean;
  accountId: string | null;
  message: string | null;
}

const desktopSessionId = `vcad_${crypto.randomUUID()}`;

type CodexNativeEvent =
  | { kind: "line"; line: string }
  | { kind: "done" }
  | { kind: "error"; message: string };

export async function getCodexAuthStatus(): Promise<CodexAuthStatus> {
  if (!isTauri()) {
    return {
      available: false,
      loggedIn: false,
      accountId: null,
      message: "Codex subscription transport is desktop-only",
    };
  }
  return invoke<CodexAuthStatus>("codex_auth_status");
}

function requestId(): string {
  return `vcad_${Date.now()}_${Math.random().toString(36).slice(2, 10)}`;
}

export async function streamCodexChat(
  messages: ChatRequestMessage[],
  callbacks: CodexStreamCallbacks,
  options: CodexRequestOptions & { signal?: AbortSignal; sessionId?: string | null },
): Promise<void> {
  if (!isTauri()) {
    callbacks.onError("Codex subscription transport is only available in the desktop app");
    callbacks.onFinish();
    return;
  }

  const id = requestId();
  // Keep all passes in the same VCAD conversation (including tool-result
  // continuations) on one Codex session/cache key. A persisted VCAD thread is
  // preferred; otherwise this desktop-process id is stable for the session.
  const sessionId = options.sessionId || desktopSessionId;
  const parser = new CodexSseAccumulator(callbacks);
  const onEvent = new Channel<CodexNativeEvent>();
  let nativeError: string | null = null;

  onEvent.onmessage = (event) => {
    if (event.kind === "line") {
      parser.consumeLine(event.line);
    } else if (event.kind === "error") {
      nativeError = event.message;
      callbacks.onError(event.message);
    } else {
      parser.finish();
    }
  };

  const abort = () => {
    void invoke<void>("codex_chat_cancel", { requestId: id });
  };
  options.signal?.addEventListener("abort", abort, { once: true });

  try {
    await invoke<void>("codex_chat_stream", {
      requestId: id,
      sessionId,
      body: buildCodexRequest(messages, options),
      onEvent,
    });
  } catch (error) {
    if (!options.signal?.aborted && nativeError === null) {
      callbacks.onError(error instanceof Error ? error.message : String(error));
    }
  } finally {
    options.signal?.removeEventListener("abort", abort);
    parser.finish();
  }
}
