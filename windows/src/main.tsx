import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { applyTokens, savedTheme, setTheme } from "./tokens";
import "./styles.css";

applyTokens();
setTheme(savedTheme());

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
