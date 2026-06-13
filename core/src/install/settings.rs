// ── LLM configuration step ────────────────────────────────────────────────────

use super::ui::*;

pub(super) fn configure_llm_interactive() {
    use crate::settings::{self, EmbeddingConfig, LlmConfig};

    let want = prompt_confirm(
        "Configure an LLM for smarter knowledge distillation? (optional)",
        false,
    );
    if !want {
        return;
    }

    // Load existing settings (preserve any already-set values). Abort rather
    // than overwrite a present-but-corrupt config with defaults.
    let mut s = match settings::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping LLM config — existing settings.json is invalid: {e}");
            return;
        }
    };

    // ── Provider ──────────────────────────────────────────────────────────
    let provider_idx = prompt_select(
        "LLM API format:",
        &[
            "OpenAI  (ChatGPT / DeepSeek / Ollama / …)",
            "Anthropic  (Claude)",
        ],
    );
    let provider = if provider_idx == 1 {
        "anthropic".to_string()
    } else {
        "openai".to_string()
    };

    // ── Base URL (only ask when using OpenAI format, to allow custom endpoints) ──
    let default_base_url = match provider.as_str() {
        "anthropic" => "https://api.anthropic.com",
        _ => "https://api.openai.com/v1",
    };
    let base_url_raw = prompt_text("Base URL", default_base_url, "(leave blank for default)");
    let base_url = if base_url_raw == default_base_url || base_url_raw.is_empty() {
        None
    } else {
        Some(base_url_raw)
    };

    // ── Model ID ──────────────────────────────────────────────────────────
    let default_model = match provider.as_str() {
        "anthropic" => "claude-haiku-4-5-20251001",
        _ => "gpt-4o-mini",
    };
    let model_id = prompt_text("Model ID", default_model, "");

    // ── API key ───────────────────────────────────────────────────────────
    let env_hint = match provider.as_str() {
        "anthropic" => "(stored 0600; or set ANTHROPIC_API_KEY env var to skip)",
        _ => "(stored 0600; or set OPENAI_API_KEY env var to skip)",
    };
    let api_key_raw = prompt_secret("API key", env_hint);
    let api_key = if api_key_raw.is_empty() {
        None
    } else {
        Some(api_key_raw)
    };

    let llm_cfg = LlmConfig {
        provider: provider.clone(),
        base_url: base_url.clone(),
        model_id: model_id.clone(),
        api_key: api_key.clone(),
    };

    // ── Optional connection test ──────────────────────────────────────────
    let do_test = prompt_confirm("Test LLM connection now?", true);
    if do_test {
        info("Testing connection…");
        match crate::llm::test_llm(&llm_cfg) {
            Ok(msg) => result_line(&format!("LLM OK — {msg}")),
            Err(e) => warn_line(&format!("LLM test failed: {e}")),
        }
    }

    s.llm = Some(llm_cfg);

    // ── Embedding model (optional, defaults to same API as LLM) ──────────
    let want_embed = prompt_confirm(
        "Configure an embedding model too? (enables semantic recall — optional)",
        false,
    );
    if want_embed {
        let embed_default_url = base_url
            .clone()
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let embed_base_url_raw = prompt_text(
            "Embedding base URL",
            &embed_default_url,
            "(leave blank to reuse LLM base URL)",
        );
        let embed_base_url =
            if embed_base_url_raw == embed_default_url || embed_base_url_raw.is_empty() {
                base_url.clone()
            } else {
                Some(embed_base_url_raw)
            };

        let embed_model = prompt_text("Embedding model ID", "text-embedding-3-small", "");

        let embed_dim_str = prompt_text(
            "Embedding dimension",
            "1536",
            "(model-specific: text-embedding-3-small=1536, qwen3-embedding=2560)",
        );
        let mut embed_dim: usize = embed_dim_str.parse().unwrap_or(1536);

        let embed_key_raw =
            prompt_secret("Embedding API key", "(leave blank to reuse LLM API key)");
        let embed_key = if embed_key_raw.is_empty() {
            api_key.clone()
        } else {
            Some(embed_key_raw)
        };

        let embed_cfg = EmbeddingConfig {
            provider: "openai".to_string(),
            base_url: embed_base_url,
            model_id: embed_model.clone(),
            api_key: embed_key.clone(),
            dim: embed_dim,
        };

        let do_test_embed = prompt_confirm("Test embedding connection now?", true);
        if do_test_embed {
            info("Testing embedding…");
            match crate::llm::test_embedding(&embed_cfg) {
                Ok(detected_dim) => {
                    result_line(&format!(
                        "Embedding OK — dim={detected_dim} model={embed_model}"
                    ));
                    if detected_dim != embed_dim {
                        info(&format!(
                            "Auto-corrected dim: {} → {}",
                            embed_dim, detected_dim
                        ));
                        embed_dim = detected_dim;
                    }
                }
                Err(e) => warn_line(&format!("Embedding test failed: {e}")),
            }
        }

        s.embedding = Some(EmbeddingConfig {
            dim: embed_dim,
            ..embed_cfg
        });
    }

    // ── Save settings ─────────────────────────────────────────────────────
    match settings::save(&s) {
        Ok(()) => result_line(&format!(
            "LLM config saved to {}",
            bold(&settings::settings_path().display().to_string())
        )),
        Err(e) => warn_line(&format!("Could not save settings: {e}")),
    }
    sep();
}

// ── Daemon configuration step ─────────────────────────────────────────────────

pub(super) fn configure_daemon_interactive() {
    use crate::settings::{self, DaemonConfig};

    // Auto-create the default hooks directory.
    let hooks_dir = crate::paths::sessions_dir();
    let _ = std::fs::create_dir_all(&hooks_dir);
    let default_watch = "~/.innate/sessions".to_string();

    let want = prompt_confirm(
        "Enable daemon auto-collection? (auto-evolves knowledge after each session)",
        true,
    );
    if !want {
        return;
    }

    info(&format!(
        "Default watch dir: {}  (created automatically)",
        bold(&default_watch)
    ));
    info("Add more directories below, or press Enter to use the default only.");
    sep();

    let mut watch_dirs: Vec<String> = vec![default_watch];
    loop {
        let dir = prompt_text(
            "Additional watch directory",
            "",
            "(leave blank to finish, ~ is expanded)",
        );
        if dir.is_empty() {
            break;
        }
        watch_dirs.push(dir.clone());
        result_line(&format!("Added: {dir}"));
    }

    let mut s = match settings::load() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Skipping daemon config — existing settings.json is invalid: {e}");
            return;
        }
    };
    s.daemon = Some(DaemonConfig {
        watch_dirs,
        auto_start: true,
    });

    match settings::save(&s) {
        Ok(()) => result_line(&format!(
            "Daemon config saved to {}",
            bold(&settings::settings_path().display().to_string())
        )),
        Err(e) => warn_line(&format!("Could not save daemon config: {e}")),
    }
    sep();
}
