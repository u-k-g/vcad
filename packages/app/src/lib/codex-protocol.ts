import type { AnthropicTool } from "@vcad/core";
import type { ChatRequestMessage, ToolCall } from "@/lib/chat-api";

export interface CodexRequestOptions {
  tools?: AnthropicTool[];
  systemPrompt?: string;
  model?: string;
}

export interface CodexStreamCallbacks {
  onText: (fullText: string) => void;
  onToolCall: (tool: ToolCall) => void;
  onProviderItem?: (item: Record<string, unknown>) => void;
  onUsage?: (usage: { inputTokens: number; outputTokens: number }) => void;
  onError: (error: string) => void;
  onFinish: () => void;
}

type JsonObject = Record<string, unknown>;

function object(value: unknown): JsonObject | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as JsonObject)
    : null;
}

function dataUrlFromAnthropicSource(source: unknown): string | null {
  const value = object(source);
  if (
    value?.type !== "base64" ||
    typeof value.media_type !== "string" ||
    typeof value.data !== "string"
  ) {
    return null;
  }
  return `data:${value.media_type};base64,${value.data}`;
}

function toolOutput(content: unknown, isError: boolean): string | JsonObject[] {
  if (typeof content === "string") {
    return isError ? `Tool error: ${content}` : content;
  }
  if (!Array.isArray(content)) return JSON.stringify(content ?? "");

  const output: JsonObject[] = [];
  for (const raw of content) {
    const block = object(raw);
    if (!block) continue;
    if (block.type === "text" && typeof block.text === "string") {
      output.push({
        type: "input_text",
        text: isError ? `Tool error: ${block.text}` : block.text,
      });
    } else if (block.type === "image") {
      const imageUrl = dataUrlFromAnthropicSource(block.source);
      if (imageUrl) output.push({ type: "input_image", image_url: imageUrl });
    }
  }
  return output.length > 0 ? output : JSON.stringify(content);
}

/** Convert VCAD's existing Anthropic-shaped history into Responses API items. */
export function codexInputFromMessages(messages: ChatRequestMessage[]): JsonObject[] {
  const input: JsonObject[] = [];

  for (const message of messages) {
    // Encrypted reasoning returned by a prior Codex response must travel back
    // with the function-call output. It remains opaque to VCAD.
    for (const providerItem of message.providerItems ?? []) {
      input.push(providerItem);
    }

    if (typeof message.content === "string") {
      if (!message.content.trim()) continue;
      input.push({
        role: message.role,
        content: [
          {
            type: message.role === "assistant" ? "output_text" : "input_text",
            text: message.content,
          },
        ],
      });
      continue;
    }

    let textAndImages: JsonObject[] = [];
    const flushMessage = () => {
      if (textAndImages.length === 0) return;
      input.push({ role: message.role, content: textAndImages });
      textAndImages = [];
    };

    for (const raw of message.content) {
      const block = object(raw);
      if (!block || typeof block.type !== "string") continue;

      if (block.type === "text" && typeof block.text === "string") {
        textAndImages.push({
          type: message.role === "assistant" ? "output_text" : "input_text",
          text: block.text,
        });
      } else if (block.type === "image" && message.role === "user") {
        const imageUrl = dataUrlFromAnthropicSource(block.source);
        if (imageUrl) {
          textAndImages.push({
            type: "input_image",
            image_url: imageUrl,
            detail: "auto",
          });
        }
      } else if (block.type === "tool_use" && message.role === "assistant") {
        flushMessage();
        input.push({
          type: "function_call",
          call_id: String(block.id ?? ""),
          name: String(block.name ?? ""),
          arguments: JSON.stringify(object(block.input) ?? {}),
        });
      } else if (block.type === "tool_result" && message.role === "user") {
        flushMessage();
        input.push({
          type: "function_call_output",
          call_id: String(block.tool_use_id ?? ""),
          output: toolOutput(block.content, block.is_error === true),
        });
      }
    }
    flushMessage();
  }

  return input;
}

/** Build the direct Codex Responses request while keeping VCAD's prompt/tools unchanged. */
export function buildCodexRequest(
  messages: ChatRequestMessage[],
  options: CodexRequestOptions,
): JsonObject {
  return {
    model: options.model ?? "gpt-5.5",
    instructions: options.systemPrompt ?? "",
    input: codexInputFromMessages(messages),
    tools: (options.tools ?? []).map((tool) => ({
      type: "function",
      name: tool.name,
      description: tool.description,
      parameters: tool.input_schema,
      strict: false,
    })),
    tool_choice: "auto",
    parallel_tool_calls: true,
    reasoning: { effort: "medium", summary: "auto" },
    include: ["reasoning.encrypted_content"],
    store: false,
    stream: true,
  };
}

function errorMessage(event: JsonObject): string {
  const error = object(event.error) ?? object(object(event.response)?.error);
  if (typeof error?.message === "string") return error.message;
  const incomplete = object(object(event.response)?.incomplete_details);
  if (typeof incomplete?.reason === "string") {
    return `Codex response incomplete: ${incomplete.reason}`;
  }
  return "Codex response failed";
}

/** Stateful parser for the data lines emitted by the Responses SSE stream. */
export class CodexSseAccumulator {
  private fullText = "";
  private finished = false;

  constructor(private readonly callbacks: CodexStreamCallbacks) {}

  consumeLine(line: string): void {
    if (!line.startsWith("data:")) return;
    const data = line.slice(5).trimStart();
    if (!data || data === "[DONE]") return;

    let event: JsonObject;
    try {
      const parsed = JSON.parse(data) as unknown;
      const parsedObject = object(parsed);
      if (!parsedObject) return;
      event = parsedObject;
    } catch {
      return;
    }

    switch (event.type) {
      case "response.output_text.delta":
        if (typeof event.delta === "string") {
          this.fullText += event.delta;
          this.callbacks.onText(this.fullText);
        }
        break;
      case "response.output_item.done": {
        const item = object(event.item);
        if (!item) break;
        if (item.type === "reasoning" && typeof item.encrypted_content === "string") {
          this.callbacks.onProviderItem?.(item);
        } else if (
          item.type === "function_call" &&
          typeof item.call_id === "string" &&
          typeof item.name === "string"
        ) {
          let args: Record<string, unknown> = {};
          if (typeof item.arguments === "string") {
            try {
              const parsed = JSON.parse(item.arguments) as unknown;
              args = object(parsed) ?? {};
            } catch {
              // The existing VCAD agent treats malformed/empty arguments as {}.
            }
          }
          this.callbacks.onToolCall({
            id: item.call_id,
            name: item.name,
            args,
          });
        }
        break;
      }
      case "response.completed": {
        const usage = object(object(event.response)?.usage);
        this.callbacks.onUsage?.({
          inputTokens:
            typeof usage?.input_tokens === "number" ? usage.input_tokens : 0,
          outputTokens:
            typeof usage?.output_tokens === "number" ? usage.output_tokens : 0,
        });
        break;
      }
      case "response.failed":
      case "response.incomplete":
      case "error":
        this.callbacks.onError(errorMessage(event));
        break;
    }
  }

  finish(): void {
    if (this.finished) return;
    this.finished = true;
    this.callbacks.onFinish();
  }
}
