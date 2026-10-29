import { WebGLRenderer } from "three";
import { editorReady } from "./editor/index.ts";
import { overlaysReady } from "./overlays/index.ts";
import { panelsReady } from "./panels/index.ts";
import { renderReady } from "./render/index.ts";
import { workerReady } from "./worker/index.ts";
import "./style.css";

const canvas = document.querySelector<HTMLCanvasElement>("#viewport");
if (
  canvas === null ||
  !renderReady ||
  !editorReady ||
  !overlaysReady ||
  !panelsReady ||
  !workerReady
) {
  throw new Error("viewport scaffold failed to load");
}

const renderer = new WebGLRenderer({ canvas, antialias: true });
renderer.setPixelRatio(window.devicePixelRatio);
renderer.setSize(window.innerWidth, window.innerHeight);
renderer.setClearColor(0x142036);

window.addEventListener("resize", () => {
  renderer.setSize(window.innerWidth, window.innerHeight);
});
