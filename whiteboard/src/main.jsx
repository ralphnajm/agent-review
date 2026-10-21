// The whiteboard: Excalidraw over a screenshot of the product.
//
// The app drives it through `window.board` and hears back through the
// `whiteboard` script message handler, as JSON: `{ "ready": true }` once it
// can take a screenshot, then `{ tool, drawing, undo, redo }` whenever one of
// them changes, and `{ "action": "done" }` or `{ "action": "discard" }` for
// Ctrl+Enter and Esc. Excalidraw's own toolbar and menus are hidden: the app's bar
// has the tools, and the panel of the selected element stays.

import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  CaptureUpdateAction,
  Excalidraw,
  FONT_FAMILY,
  convertToExcalidrawElements,
  exportToBlob,
  newElementWith,
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
/** A sticky note's side, at a regular screen's scale. */
const STICKY_SIDE = 180;

let api = null;
/** The screenshot: its id among Excalidraw's files, its element, and how many of its pixels make a pixel of the scene. */
let screenshot = null;
/** The colour the user picked, for what they draw next. */
let color = "#e01b24";
/** While a new sticky note's text is being typed, which turns the ink dark: whether typing began. */
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
  // The panel of the selected element only shows while something is selected, or text is typed.
  const selecting = Object.keys(appState.selectedElementIds).length > 0 || appState.editingTextElement;
  document.body.classList.toggle("selecting", Boolean(selecting));
  if (stickyTyping) {
    if (appState.editingTextElement) {
      stickyTyping.began = true;
    } else if (stickyTyping.began) {
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
  stickyTyping = { began: false };
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
        // Once laid out, the screenshot fills the board. Excalidraw sets its tool up by then too.
        requestAnimationFrame(() =>
          requestAnimationFrame(() => {
            api.setActiveTool(TOOLS[scene.tool]);
            const element = api.getSceneElements().find((e) => e.id === screenshot.element);
            api.scrollToContent(element, { fitToViewport: true, viewportZoomFactor: 1, animate: false });
          }),
        );
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

window.board = {
  /** Starts over on a screenshot, given as a PNG `data:` URL, with `tool`. */
  async load(png, tool) {
    const [width, height] = await imageSize(png);
    // A regular screen's pixels, so lines and text look the same on high-resolution screenshots.
    const scale = Math.max(1, width / 1400);
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
          currentItemStrokeColor: color,
          currentItemStrokeWidth: 4,
          currentItemRoughness: 0,
          currentItemFontFamily: FONT_FAMILY.Nunito,
          currentItemFontSize: 20,
          viewBackgroundColor: "#ffffff",
        },
      },
    });
  },

  setTool(tool) {
    api?.setActiveTool(TOOLS[tool]);
  },

  /** What the user draws next takes `hex`, and so do the selected elements. */
  setColor(hex) {
    color = hex;
    if (!api) {
      return;
    }
    const appState = api.getAppState();
    const elements = api.getSceneElementsIncludingDeleted();
    // A sticky note keeps its paper: its text takes the colour.
    const stickies = new Set(elements.filter((e) => e.customData?.sticky).map((e) => e.id));
    const selected = (e) =>
      appState.selectedElementIds[e.id] || (e.containerId && stickies.has(e.containerId) && appState.selectedElementIds[e.containerId]);
    const changed = elements.some((e) => selected(e) && !stickies.has(e.id));
    api.updateScene({
      elements: changed
        ? elements.map((e) => (selected(e) && !stickies.has(e.id) ? newElementWith(e, { strokeColor: hex }) : e))
        : undefined,
      appState: { currentItemStrokeColor: hex },
      captureUpdate: changed ? CaptureUpdateAction.IMMEDIATELY : CaptureUpdateAction.NEVER,
    });
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
// else to stop first, such as typing or a selection. Ctrl+Y is Redo too.
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

createRoot(document.getElementById("root")).render(<Board />);
