# Runtime system prompt

Every model request initiated by Rynna's core carries a shared software-development and terminal-work policy. It asks the model to inspect before editing, make focused changes, test and verify with actual results, respect execution permissions, and report uncertainty or blockers rather than invent success.

The policy uses **Reason -> Act -> Observe**: reason step by step privately, use an available tool when needed, inspect its result, and repeat until complete. Private chain-of-thought is not requested as output; the model is instructed to provide concise conclusions, evidence, and useful reasoning summaries instead. This is a prompt instruction, not a new reasoning API or a guarantee that a model will follow it. Existing provider thinking controls and streaming behavior are unchanged.

## Composition and tool availability

The first system message contains:

1. The shared core policy.
2. The configured system prompt, including any project and active-skill context already attached by the caller.
3. The existing YOLO instructions, when enabled.
4. A JSON list headed `Available tools for this request (JSON):`, built directly from that request's tool definitions.

Configured prompts supplement the shared policy rather than replacing it. Task-specific system instructions, such as title-only or summary-only output requirements, remain separate system messages. User messages, recalled memory, conversation summaries, and tool results are not promoted to system instructions.

The inventory names exactly the tools supplied on the current request, not every installed capability. Native-tool filtering, discovered MCP tools, selected provider capabilities, and delegation restrictions are applied before it is generated. Descriptions and argument schemas remain in the structured tool definitions. `[]` explicitly means no tools are callable on this request. In particular, a final-answer-only retry does not retain the preceding tool-enabled inventory.

## Covered paths

The shared constructor in `crates/rynna-core/src/system_prompt.rs` is used for normal and streaming agent turns, tool follow-ups, empty-answer retries, manual/automatic rolling summaries, and session titles. Subagents and workflow steps reuse the same agent path, retaining their role/task instructions without accumulating copies of the policy. Provider fallback and transport retries reuse the composed request. Session titles and summaries keep their specialized output restrictions and never gain tools merely because their profile has tools.

Policy and inventory are composed before context estimation. After an injected context manager prepares a request, core reconciles them again with the resulting tools and accounts for any changed message size before dispatch. Conversation-size estimates include the shared policy and inventory; as before, dynamically discovered tools and recalled memory can increase usage during execution. Workflow policy fingerprints include the shared policy, so a policy change invalidates an older resume fingerprint.

Provider adapters continue to transport system messages using their existing mechanisms. They do not own or duplicate this policy. Direct consumers of the low-level `ModelProvider` trait are responsible for their own requests; application features should enter through core orchestration rather than bypassing it.

## Tests

`cargo test -p rynna-core` covers prompt composition, configured context, title/summary restrictions, custom context management, native/discovered/disabled tools, providers without tool support, parent/helper inventories, workflow dispatch, streaming final-answer retries, and fallback attempts. The CLI chat integration tests also verify the policy survives transport while history and configured instructions are preserved.
