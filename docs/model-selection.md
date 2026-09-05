# In-chat model selection

CLI chat, the desktop app, and the web UI can choose an enabled provider/model pair from the current profile and set its thinking level. Changes apply to subsequent messages in the current conversation. History, skills, tools, memory settings, and saved profile defaults are preserved.

In desktop and web chat, click the model and thinking label at the bottom right of the message box. The picker opens above the composer with searchable models grouped by provider and **Thinking level** controls at the bottom. Select **Profile default** to restore the profile's normal default and fallback order. Choosing a model explicitly pins requests to that pair, so a provider error does not silently switch to a different model. Controls are disabled while a response is running. Changing provider or model resets thinking to Default; changing profile clears the selection.

In interactive terminal chat, click the model label on the bottom-right composer border or press **F2** (or enter `/model`) to open the picker. Type to search models or providers, use **↑/↓** or the mouse wheel to browse, and press **Enter** or click a model to select it. **←/→** or **Tab/Shift-Tab** cycles thinking levels; the levels are also clickable. **Esc** or a click outside closes the picker and preserves your draft. **Profile default** restores normal routing. Selection is unavailable while a response is running.

Both terminal and plain-text CLI chat also support these commands (plain-text `/model` lists choices instead of opening the picker):

- `/model` lists numbered enabled provider/model pairs.
- `/model 2` selects the second listed pair.
- `/model <provider> <model>` selects an exact pair.
- `/provider <provider>` selects that provider's first enabled model.
- `/thinking default|low|medium|high` sets effort. If no pair is selected, this pins the profile's default pair.
- `/model default` restores profile defaults and automatic fallback.

Selector commands do not become chat history. Invalid selections report an error and leave the previous selection intact. Add or enable additional pairs in the profile's model settings before choosing them in chat.

## Thinking levels

**Default** omits an effort override. **Low**, **Medium**, and **High** use the provider's native effort control:

- OpenAI-compatible chat APIs, including OpenRouter and Ollama: `reasoning_effort`.
- OpenAI managed Responses: `reasoning.effort`.
- Anthropic Messages: adaptive `thinking` with `output_config.effort`.
- Claude subscription: Claude Code `--effort`.
- ChatGPT subscription: Codex app-server `turn/start` `effort`.

Thinking support depends on the provider and model. For example, older Claude models may not support adaptive thinking, and some Ollama models support only on/off thinking. Provider errors are shown in chat; use Default when a model does not support the requested effort. Rynna does not translate effort into arbitrary token budgets. The existing pinned subscription CLI versions still apply.

## HTTP and desktop requests

Both synchronous and streaming response requests accept an optional `selection` object:

```json
{
  "profile": "work",
  "prompt": "Continue reviewing this change",
  "history": [],
  "selection": {
    "provider": "openai",
    "model": "your-enabled-model-id",
    "thinking": "high"
  }
}
```

Omitting `selection` preserves existing request behavior. Omitting `thinking` uses Default. Unknown effort values and pairs not enabled in the requested profile are rejected. Selection operates on a request-local runtime snapshot, leaving other conversations and persisted settings unchanged. Selected requests rebuild provider-managed context from the supplied conversation history rather than reusing opaque continuations from a different model or effort.
