import { render } from "preact";
import { Desktop } from "./desktop/Desktop.js";
import "./style.css";

const root = document.getElementById("app");
if (!root) throw new Error("#app missing from index.html");

render(<Desktop />, root);
