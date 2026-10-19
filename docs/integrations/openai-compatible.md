# OpenAI-compatible endpoints

llama.cpp `llama-server`, Halogen, vLLM, SGLang, Ollama (`/v1`), LM Studio, TGI, and cloud APIs
that speak `/chat/completions`.

```toml
[table.player]
kind = "openai"
base_url = "http://127.0.0.1:8080/v1"   # default
model = "local-model"                   # what your server expects in "model"
api_key_env = "OPENAI_API_KEY"          # name of the env var; omit for local servers
system = "You are a careful Rust programmer."
temperature = 0.2
max_tokens = 4096
timeout_secs = 900
halogen_keep = false
extra = { top_p = 0.9 }                 # merged into every request body
```

The conversation is kept in memory for the table's life: rally N sends the brief, every answer
and every return message so far, which is a growing *prefix*: servers with prompt caching
(llama.cpp slots, vLLM prefix cache, Halogen) only process the new return message. `<think>`
blocks are stripped from the answer the referee sees (they stay in the history). Usage
(`prompt_tokens`, `completion_tokens`) counts against `max_tokens`.

A chat model cannot edit files, so pair it with `apply`:

```toml
apply = { write_file = "src/lib.rs" }          # first fenced code block of the answer
ball = "Fix this file:\n```rust\n{{file:src/lib.rs}}\n```"
```

## Halogen keep/fork

With `halogen_keep = true` the player sends `X-Halogen-Keep: upp-<table>` on every rally and
`X-Halogen-From: upp-<table>` from rally 2 on, so a Halogen server started with `HALOGEN_FORK=1`
forks the stored state of the previous turn instead of matching the prefix. Handles are
1-64 characters of `[A-Za-z0-9_.:-]`.

**Status: tested live** on Halogen 0.18.11 (Qwen 3.8 Flash Next) with `halogen_keep = true`: one
table, `fizzbuzz` from [three-functions.toml](../../examples/three-functions.toml), point on
rally 1 in 42 s, 992 tokens, `cargo test` referee.
