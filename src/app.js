const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const { open } = window.__TAURI__.dialog;
const { getCurrentWebview } = window.__TAURI__.webview;
const { revealItemInDir } = window.__TAURI__.opener;

const FORMATS = ["mp3", "m4a", "opus", "ogg", "flac", "wav"];
const BITRATES = ["128k", "192k", "256k", "320k"];
const HEIGHTS = ["mejor", "2160", "1440", "1080", "720", "480"];
// Los formatos sin pérdida ignoran el bitrate.
const LOSSLESS = ["flac", "wav"];

const $ = (id) => document.getElementById(id);

// Las rutas largas no caben. Se enseñan las dos últimas carpetas y la completa
// queda en el tooltip. Vale para separadores de Windows y de macOS.
const shortPath = (p) => {
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length > 2 ? "…/" + parts.slice(-2).join("/") : p;
};
const fill = (el, items, i = 0) => {
  el.innerHTML = items.map((x) => `<option>${x}</option>`).join("");
  el.selectedIndex = i;
};

let files = [];
let convertOut = "";
let downloadOut = "";
let busy = false;
let lastOut = "";
let tab = "convert";

// ---------------------------------------------------------------- arranque

fill($("cFormat"), FORMATS);
fill($("cBitrate"), BITRATES, 1);
fill($("dFormat"), FORMATS);
fill($("dBitrate"), BITRATES, 1);
fill($("dHeight"), HEIGHTS, 3);

invoke("default_out").then((p) => {
  downloadOut = p;
  render();
});
refreshTools();

// ---------------------------------------------------------------- pestañas

document.querySelectorAll(".tabs button").forEach((b) => {
  b.onclick = () => {
    tab = b.dataset.tab;
    document.querySelectorAll(".tabs button").forEach((x) => x.classList.toggle("active", x === b));
    $("tab-convert").classList.toggle("hidden", tab !== "convert");
    $("tab-download").classList.toggle("hidden", tab !== "download");
    render();
  };
});

// ------------------------------------------------------------ herramientas

async function refreshTools() {
  showTools(await invoke("tool_status"));
}

function showTools(t) {
  const missing = [!t.ffmpeg && "ffmpeg", !t.ytdlp && "yt-dlp"].filter(Boolean);
  const box = $("tools");
  box.classList.toggle("hidden", missing.length === 0);
  if (!missing.length) return;
  box.innerHTML = `Falta ${missing.join(" y ")} <button id="getTools" class="link">Descargar herramientas</button>`;
  $("getTools").onclick = async () => {
    try {
      await invoke("install_tools");
      $("log").classList.remove("hidden");
    } catch (e) {
      alert(e);
    }
  };
}

listen("tools", (e) => showTools(e.payload));

// ------------------------------------------------------------ lista de archivos

function render() {
  $("list").innerHTML = files
    .map(
      (f, i) =>
        `<li><span class="nm"></span><span class="dr"></span>
         <button class="icon" data-i="${i}" title="Quitar">✕</button></li>`
    )
    .join("");
  // El nombre va por textContent: un archivo puede llamarse "<script>".
  $("list").querySelectorAll("li").forEach((li, i) => {
    const nm = li.querySelector(".nm");
    const dr = li.querySelector(".dr");
    nm.textContent = files[i].name;
    nm.title = files[i].path;
    dr.textContent = shortPath(files[i].dir);
    dr.title = files[i].dir;
  });
  $("list").querySelectorAll(".icon").forEach((b) => {
    b.onclick = () => {
      files.splice(+b.dataset.i, 1);
      render();
    };
  });

  $("cOut").textContent = convertOut ? shortPath(convertOut) : "Junto al archivo original";
  $("cOut").title = convertOut || "El audio se guarda en la misma carpeta que el vídeo";
  $("cOutClear").classList.toggle("hidden", !convertOut);
  $("dOut").textContent = shortPath(downloadOut);
  $("dOut").title = downloadOut;
  const convirtiendo = tab === "convert";
  $("go").textContent = busy
    ? "Cancelar"
    : convirtiendo
      ? files.length
        ? `Convertir ${files.length} archivo(s)`
        : "Convertir"
      : "Descargar";
  $("go").classList.toggle("cancel", busy);

  const lossless = LOSSLESS.includes($("cFormat").value);
  $("cBitrate").disabled = lossless;
  $("dBitrate").disabled = LOSSLESS.includes($("dFormat").value);
}

async function add(paths) {
  if (!paths || !paths.length) return;
  const known = files.map((f) => f.path);
  files = await invoke("scan", { paths: [...known, ...paths] });
  render();
}

$("addFiles").onclick = async () =>
  add(await open({ multiple: true, title: "Elegir vídeos" }));

$("addFolder").onclick = async () => {
  const d = await open({ directory: true, multiple: true, title: "Elegir carpeta" });
  add(Array.isArray(d) ? d : d ? [d] : []);
};

$("cOut").onclick = async () => {
  const d = await open({ directory: true, title: "Carpeta de destino" });
  if (d) {
    convertOut = d;
    render();
  }
};

$("cOutClear").onclick = () => {
  convertOut = "";
  render();
};

$("dOut").onclick = async () => {
  const d = await open({ directory: true, title: "Carpeta de destino" });
  if (d) {
    downloadOut = d;
    render();
  }
};

$("cFormat").onchange = render;
$("dFormat").onchange = render;

$("dKind").onchange = () => {
  const audio = $("dKind").value === "audio";
  document.querySelectorAll(".audio-only").forEach((e) => e.classList.toggle("hidden", !audio));
  document.querySelectorAll(".video-only").forEach((e) => e.classList.toggle("hidden", audio));
};

// --------------------------------------------------------- arrastrar y soltar

getCurrentWebview().onDragDropEvent(({ payload }) => {
  const drop = $("drop");
  if (payload.type === "over") drop.classList.add("over");
  else drop.classList.remove("over");
  if (payload.type === "drop") add(payload.paths);
});

// ------------------------------------------------------------------ acciones

$("go").onclick = async () => {
  if (busy) return invoke("cancel");
  try {
    if (tab === "convert") {
      await invoke("convert", {
        files: files.map((f) => f.path),
        format: $("cFormat").value,
        bitrate: $("cBitrate").value,
        outdir: convertOut,
      });
      lastOut = convertOut || files[0]?.dir || "";
    } else {
      await invoke("download", {
        url: $("url").value,
        audio: $("dKind").value === "audio",
        format: $("dFormat").value,
        bitrate: $("dBitrate").value,
        height: $("dHeight").value,
        outdir: downloadOut,
      });
      lastOut = downloadOut;
    }
  } catch (e) {
    alert(e);
  }
};

$("openOut").onclick = () => revealItemInDir(lastOut).catch((e) => alert(e));

$("toggleLog").onclick = () => {
  const hidden = $("log").classList.toggle("hidden");
  $("toggleLog").textContent = hidden ? "Ver detalle" : "Ocultar detalle";
};

// -------------------------------------------------------------------- eventos

listen("log", ({ payload }) => {
  const el = $("log");
  el.textContent += payload + "\n";
  el.scrollTop = el.scrollHeight;
});

listen("progress", ({ payload }) => {
  $("bar").style.width = `${Math.round(payload.value * 100)}%`;
  $("status").textContent = payload.status;
});

listen("busy", ({ payload }) => {
  busy = payload;
  render();
  if (!busy) {
    refreshTools();
    $("openOut").classList.toggle("hidden", !lastOut);
  }
});
