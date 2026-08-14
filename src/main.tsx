import React from "react";
import ReactDOM from "react-dom/client";
import "./index.css";
import App from "./App";
import { ThemeProvider } from "./contexts/ThemeContext";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { installCrashReporting } from "./lib/crash-reporting";

installCrashReporting();

// The boundary sits outside ThemeProvider rather than inside App: a throw from
// the provider itself, or from anything it does on mount, used to escape the
// boundary entirely and leave a blank window with nothing written anywhere.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <ThemeProvider defaultTheme="explorer">
        <App />
      </ThemeProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);
