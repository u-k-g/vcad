import { describe, expect, it, vi } from "vitest";
import {
  buildCodexRequest,
  codexInputFromMessages,
  CodexSseAccumulator,
} from "@/lib/codex-protocol";

describe("Codex subscription protocol adapter", () => {
  it("preserves VCAD prompts and converts tool schemas", () => {
    const request = buildCodexRequest(
      [{ role: "user", content: "make a cube" }],
      {
        systemPrompt: "VCAD SYSTEM PROMPT",
        tools: [
          {
            name: "create",
            description: "Create CAD geometry",
            input_schema: {
              type: "object",
              properties: { type: { type: "string" } },
            },
          },
        ],
      },
    );

    expect(request.instructions).toBe("VCAD SYSTEM PROMPT");
    expect(request.model).toBe("gpt-5.5");
    expect(request.tools).toEqual([
      {
        type: "function",
        name: "create",
        description: "Create CAD geometry",
        parameters: {
          type: "object",
          properties: { type: { type: "string" } },
        },
        strict: false,
      },
    ]);
  });

  it("converts VCAD tool calls/results and replays encrypted reasoning", () => {
    const reasoning = {
      type: "reasoning",
      id: "rs_1",
      summary: [],
      encrypted_content: "opaque",
    };
    const input = codexInputFromMessages([
      { role: "user", content: "make a cube" },
      {
        role: "assistant",
        providerItems: [reasoning],
        content: [
          { type: "text", text: "Working on it." },
          {
            type: "tool_use",
            id: "call_1",
            name: "create",
            input: { type: "cube", width: 20 },
          },
        ],
      },
      {
        role: "user",
        content: [
          {
            type: "tool_result",
            tool_use_id: "call_1",
            content: "created cube",
            is_error: false,
          },
        ],
      },
    ]);

    expect(input).toContainEqual(reasoning);
    expect(input).toContainEqual({
      type: "function_call",
      call_id: "call_1",
      name: "create",
      arguments: JSON.stringify({ type: "cube", width: 20 }),
    });
    expect(input).toContainEqual({
      type: "function_call_output",
      call_id: "call_1",
      output: "created cube",
    });
  });

  it("streams text, tool calls, reasoning, and usage", () => {
    const onText = vi.fn();
    const onToolCall = vi.fn();
    const onProviderItem = vi.fn();
    const onUsage = vi.fn();
    const onError = vi.fn();
    const onFinish = vi.fn();
    const parser = new CodexSseAccumulator({
      onText,
      onToolCall,
      onProviderItem,
      onUsage,
      onError,
      onFinish,
    });

    parser.consumeLine(
      `data: ${JSON.stringify({ type: "response.output_text.delta", delta: "Hello" })}`,
    );
    parser.consumeLine(
      `data: ${JSON.stringify({
        type: "response.output_item.done",
        item: {
          type: "reasoning",
          id: "rs_1",
          summary: [],
          encrypted_content: "opaque",
        },
      })}`,
    );
    parser.consumeLine(
      `data: ${JSON.stringify({
        type: "response.output_item.done",
        item: {
          type: "function_call",
          call_id: "call_1",
          name: "create",
          arguments: '{"type":"cube"}',
        },
      })}`,
    );
    parser.consumeLine(
      `data: ${JSON.stringify({
        type: "response.completed",
        response: { usage: { input_tokens: 10, output_tokens: 4 } },
      })}`,
    );
    parser.finish();
    parser.finish();

    expect(onText).toHaveBeenCalledWith("Hello");
    expect(onProviderItem).toHaveBeenCalledWith(
      expect.objectContaining({ type: "reasoning", encrypted_content: "opaque" }),
    );
    expect(onToolCall).toHaveBeenCalledWith({
      id: "call_1",
      name: "create",
      args: { type: "cube" },
    });
    expect(onUsage).toHaveBeenCalledWith({ inputTokens: 10, outputTokens: 4 });
    expect(onError).not.toHaveBeenCalled();
    expect(onFinish).toHaveBeenCalledTimes(1);
  });
});
