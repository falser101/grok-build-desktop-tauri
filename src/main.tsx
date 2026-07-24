import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Component, type ReactNode, type ErrorInfo } from "react";
import { App } from "./App";
import { PrefsProvider } from "./PrefsContext";
import { bootstrapAppearance } from "./prefs";
import "./desktop"; // install `window.desktop` BEFORE App mounts, so any
                    // useEffect that calls window.desktop.* doesn't hit undefined.
import "./styles.css";

bootstrapAppearance();

// ───────────────────────── top-level error boundary ─────────────────────────
//
// If React throws during render or commit (e.g., a hook tries to read
// `window.desktop.foo` synchronously), we want a visible diagnostic
// instead of an empty page. This boundary wraps the whole tree.

class BootErrorBoundary extends Component<
  { children: ReactNode },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    // eslint-disable-next-line no-console
    console.error("[boot] React crashed:", error, info);
  }

  render() {
    if (this.state.error) {
      const e = this.state.error;
      return (
        <div
          style={{
            position: "fixed",
            inset: 0,
            padding: 24,
            fontFamily: "ui-monospace, monospace",
            color: "#f88",
            background: "#1a1a1a",
            overflow: "auto",
          }}
        >
          <div style={{ fontSize: 13, marginBottom: 12 }}>
            React crashed during boot:
          </div>
          <pre style={{ fontSize: 12, whiteSpace: "pre-wrap" }}>
            {e.name}: {e.message}
            {"\n\n"}
            {e.stack ?? ""}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

const root = document.getElementById("root");
if (!root) {
  throw new Error("#root missing");
}

createRoot(root).render(
  <StrictMode>
    <BootErrorBoundary>
      <PrefsProvider>
        <App />
      </PrefsProvider>
    </BootErrorBoundary>
  </StrictMode>,
);
