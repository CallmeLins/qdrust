import { createApp } from "vue";
import App from "./App.vue";
import "./style.css";
import { api } from "./api";
import { applyDefaultLocale } from "./i18n";

/**
 * Boot the app, learning the deployment's default language first.
 *
 * `QDRUST_DEFAULT_LOCALE` lives on the server, so the language is only known
 * after one round trip — and mounting straight away would render a Chinese
 * frame that flips to English a moment later on an `en-US` deployment. The
 * mount therefore waits for it. Best-effort: any failure (offline, a proxy
 * hiccup, a value the WebUI does not ship) keeps the built-in default, because
 * a missing language is a cosmetic problem and a missing UI is not.
 */
async function mount(): Promise<void> {
  try {
    const meta = await api.meta();
    applyDefaultLocale(meta.default_locale);
  } catch {
    /* keep the built-in default */
  }
  createApp(App).mount("#app");
}

void mount();
