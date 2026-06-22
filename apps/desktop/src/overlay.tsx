/* @refresh reload */
// Entry point for the standalone overlay HUD window (overlay.html).
import { render } from "solid-js/web";
import Overlay from "./components/Overlay";
import "./styles.css";

const root = document.getElementById("overlay-root");
if (!root) throw new Error("#overlay-root not found");
render(() => <Overlay />, root);
