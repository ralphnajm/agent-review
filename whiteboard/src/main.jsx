// The whiteboard: Excalidraw over a screenshot of the product.
//
// The app drives it through `window.board` and hears back through the
// `whiteboard` script message handler, as JSON: `{ "ready": true }` once it
// can take a screenshot, `{ "shown": true }` once it shows one, then
// `{ tool, drawing, undo, redo }` whenever one of them changes, and
// `{ "action": "done" }` or `{ "action": "discard" }` for Ctrl+Enter and Esc. Excalidraw's own toolbar and menus are hidden: the app's bar
// has the tools, and the panel of the selected element stays.

import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  CaptureUpdateAction,
  Excalidraw,
  FONT_FAMILY,
  convertToExcalidrawElements,
  exportToBlob,
  sceneCoordsToViewportCoords,
} from "@excalidraw/excalidraw";
import "@excalidraw/excalidraw/index.css";
import "./board.css";

/** The tools of the app's bar, by Excalidraw's names for them. */
const TOOLS = {
  select: { type: "selection" },
  marker: { type: "freedraw" },
  arrow: { type: "arrow" },
  box: { type: "rectangle" },
  sticky: { type: "custom", customType: "sticky" },
  text: { type: "text" },
};

const STICKY_PAPER = "#fff0a0";
const STICKY_INK = "#1e1e1e";
/** A sticky note's side, in the page's pixels. */
const STICKY_SIDE = 180;

/** What's drawn takes red, until the user picks another colour for it. */
const COLOR = "#e01b24";

let api = null;
/**
 * The screenshot: its id among Excalidraw's files, its element, and how many
 * of its pixels make one of the page's, which is a pixel of the scene.
 */
let screenshot = null;
/**
 * While a new sticky note's text is being typed, in dark ink: whether typing
 * began, and the colour to go back to after.
 */
let stickyTyping = null;
let posted = {};
let show = null;

const post = (message) => window.webkit?.messageHandlers?.whiteboard?.postMessage(JSON.stringify(message));

const toolName = (tool) =>
  Object.keys(TOOLS).find((name) => {
    const t = TOOLS[name];
    return t.type === tool.type && (t.type !== "custom" || t.customType === tool.customType);
  }) ?? "select";

/** Tells the app what changed of the tool, whether there's a drawing, and whether Undo and Redo can act. */
const report = (elements, appState) => {
  const disabled = (id) => document.querySelector(`[data-testid="button-${id}"]`)?.disabled ?? true;
  const state = {
    tool: toolName(appState.activeTool),
    drawing: elements.some((e) => !e.isDeleted && e.id !== screenshot?.element),
    undo: !disabled("undo"),
    redo: !disabled("redo"),
  };
  if (Object.keys(state).some((key) => state[key] !== posted[key])) {
    posted = state;
    post(state);
  }
};

const onChange = (elements, appState) => {
  // The screenshot stays exactly where the page was, at its size: any zoom or
  // scroll would resample it, and blur it.
  if (appState.zoom.value !== 1 || appState.scrollX !== 0 || appState.scrollY !== 0) {
    api?.updateScene({ appState: { zoom: { value: 1 }, scrollX: 0, scrollY: 0 }, captureUpdate: CaptureUpdateAction.NEVER });
  }
  // The panel of the selected element only shows while something is selected, or text is typed.
  const selecting = Object.keys(appState.selectedElementIds).length > 0 || appState.editingTextElement;
  document.body.classList.toggle("selecting", Boolean(selecting));
  if (stickyTyping) {
    if (appState.editingTextElement) {
      stickyTyping.began = true;
    } else if (stickyTyping.began) {
      const { color } = stickyTyping;
      stickyTyping = null;
      api.updateScene({ appState: { currentItemStrokeColor: color }, captureUpdate: CaptureUpdateAction.NEVER });
    }
  }
  // The undo buttons update after this change renders.
  requestAnimationFrame(() => report(elements, appState));
};

/** Places a sticky note where the user clicked, and starts typing in it. */
const onPointerUp = (tool, pointerDown) => {
  if (tool.type !== "custom" || tool.customType !== "sticky" || !screenshot) {
    return;
  }
  const side = STICKY_SIDE;
  const [note] = convertToExcalidrawElements([
    {
      type: "rectangle",
      x: pointerDown.origin.x - side / 2,
      y: pointerDown.origin.y - side / 2,
      width: side,
      height: side,
      backgroundColor: STICKY_PAPER,
      fillStyle: "solid",
      strokeColor: "transparent",
      strokeWidth: 1,
      roughness: 0,
      roundness: null,
      customData: { sticky: true },
    },
  ]);
  stickyTyping = { began: false, color: api.getAppState().currentItemStrokeColor };
  api.updateScene({
    elements: [...api.getSceneElementsIncludingDeleted(), note],
    appState: { selectedElementIds: { [note.id]: true }, currentItemStrokeColor: STICKY_INK },
    captureUpdate: CaptureUpdateAction.IMMEDIATELY,
  });
  api.setActiveTool({ type: "selection" });
  // Excalidraw starts typing in a shape on a double click, and has no other way.
  requestAnimationFrame(() => {
    const center = sceneCoordsToViewportCoords(
      { sceneX: note.x + side / 2, sceneY: note.y + side / 2 },
      api.getAppState(),
    );
    document
      .querySelector(".excalidraw__canvas.interactive")
      ?.dispatchEvent(new MouseEvent("dblclick", { bubbles: true, clientX: center.x, clientY: center.y }));
  });
};

const readAsDataURL = (blob) =>
  new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(blob);
  });

const imageSize = (url) =>
  new Promise((resolve, reject) => {
    const image = new Image();
    image.onload = () => resolve([image.naturalWidth, image.naturalHeight]);
    image.onerror = () => reject(new Error("The screenshot could not be read"));
    image.src = url;
  });

function Board() {
  const [scene, setScene] = useState(null);
  const [dark, setDark] = useState(false);
  useEffect(() => {
    show = { setScene, setDark };
    post({ ready: true });
  }, []);
  useEffect(() => {
    api?.updateScene({ appState: { theme: dark ? "dark" : "light" }, captureUpdate: CaptureUpdateAction.NEVER });
  }, [dark]);
  if (!scene) {
    return null;
  }
  return (
    <Excalidraw
      // A new screenshot starts a new board, with nothing to undo.
      key={scene.key}
      initialData={{ ...scene.data, appState: { ...scene.data.appState, theme: dark ? "dark" : "light" } }}
      excalidrawAPI={(next) => {
        api = next;
        shown(scene);
      }}
      onChange={onChange}
      onPointerUp={onPointerUp}
      // The board is the whole page.
      handleKeyboardGlobally
      detectScroll={false}
      UIOptions={{
        canvasActions: {
          changeViewBackgroundColor: false,
          clearCanvas: false,
          export: false,
          loadScene: false,
          saveAsImage: false,
          saveToActiveFile: false,
          toggleTheme: false,
        },
        tools: { image: false },
      }}
    />
  );
}

const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));

/**
 * Tells the app once the board shows the screenshot, for it to stop showing
 * its own copy over it. Excalidraw sets its tool up by then too.
 */
const shown = async (scene) => {
  const image = new Image();
  image.src = scene.data.files[screenshot.fileId].dataURL;
  await image.decode().catch(() => {});
  for (let i = 0; i < 3; i++) {
    await frame();
  }
  api.setActiveTool(TOOLS[scene.tool]);
  await new Promise((resolve) => setTimeout(resolve, 50));
  post({ shown: true });
};

window.board = {
  /**
   * Starts over on a screenshot, given as a PNG `data:` URL, with `tool`.
   * `scale` of its pixels make one of the page's: it shows as big as the
   * page, where the page was.
   */
  async load(png, scale, tool) {
    const [width, height] = await imageSize(png);
    const fileId = `screenshot-${Date.now()}`;
    const [element] = convertToExcalidrawElements([
      { type: "image", fileId, x: 0, y: 0, width: width / scale, height: height / scale, locked: true },
    ]);
    screenshot = { fileId, element: element.id, scale };
    stickyTyping = null;
    posted = {};
    show.setScene({
      key: fileId,
      tool,
      data: {
        elements: [{ ...element, status: "saved" }],
        files: { [fileId]: { id: fileId, dataURL: png, mimeType: "image/png", created: Date.now() } },
        appState: {
          currentItemStrokeColor: COLOR,
          currentItemStrokeWidth: 4,
          currentItemRoughness: 0,
          currentItemFontFamily: FONT_FAMILY.Nunito,
          currentItemFontSize: 20,
          viewBackgroundColor: "#ffffff",
          zoom: { value: 1 },
          scrollX: 0,
          scrollY: 0,
        },
      },
    });
  },

  setTool(tool) {
    api?.setActiveTool(TOOLS[tool]);
  },

  setDark(dark) {
    show?.setDark(dark);
  },

  undo() {
    document.querySelector('[data-testid="button-undo"]')?.click();
  },

  redo() {
    document.querySelector('[data-testid="button-redo"]')?.click();
  },

  /**
   * The screenshot with the drawing on it, as a base64 PNG at most
   * `maxWidth` pixels wide, and the text written on it, in order, as JSON.
   */
  async export(maxWidth) {
    // Text being typed is only kept once the editor loses focus.
    document.activeElement?.blur();
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const elements = api.getSceneElements();
    const blob = await exportToBlob({
      elements,
      files: api.getFiles(),
      appState: { ...api.getAppState(), exportBackground: true, exportWithDarkMode: false },
      mimeType: "image/png",
      exportPadding: 0,
      getDimensions: (width, height) => {
        // No bigger than the screenshot's own pixels.
        const scale = Math.min(maxWidth / width, screenshot.scale);
        return { width: width * scale, height: height * scale, scale };
      },
    });
    const png = (await readAsDataURL(blob)).replace(/^data:[^,]*,/, "");
    const texts = elements
      .filter((e) => e.type === "text")
      .map((e) => (e.originalText ?? e.text).trim())
      .filter(Boolean);
    return JSON.stringify({ png, texts });
  },
};

// The keys that finish the drawing, before Excalidraw sees them: Ctrl+Enter
// adds it to the review, and Esc throws it away, unless it has something
// else to stop first, such as typing or a selection. Ctrl+Y is Redo too, and
// Ctrl+0 shows the screenshot as big as the page.
window.addEventListener(
  "keydown",
  (event) => {
    const appState = api?.getAppState();
    if (!appState) {
      return;
    }
    let action = null;
    if (event.key === "Enter" && event.ctrlKey) {
      action = "done";
    } else if (event.key === "y" && event.ctrlKey && !event.shiftKey) {
      window.board.redo();
    } else if (
      event.key === "Escape" &&
      !appState.editingTextElement &&
      !appState.newElement &&
      !appState.multiElement &&
      !appState.contextMenu &&
      !appState.openDialog &&
      !appState.openPopup &&
      Object.keys(appState.selectedElementIds).length === 0
    ) {
      action = "discard";
    } else {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (action) {
      post({ action });
    }
  },
  true,
);

// Scrolling and pinching would zoom or move the board, which stays put.
window.addEventListener(
  "wheel",
  (event) => {
    event.preventDefault();
    event.stopPropagation();
  },
  { capture: true, passive: false },
);

createRoot(document.getElementById("root")).render(<Board />);
