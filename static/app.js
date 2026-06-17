// Loads tokens as selectable cards, gates the Send buttons behind clearing the
// game (score 100), posts /api/mint, and renders the result (note download for private).
(function () {
  const addrEl = document.getElementById("recipient-address");
  const gridEl = document.getElementById("token-grid");
  const amtEl = document.getElementById("token-amount");
  const hintEl = document.getElementById("token-amount-hint");
  const pubBtn = document.getElementById("send-public-button");
  const privBtn = document.getElementById("send-private-button");
  const statusEl = document.getElementById("game-status");
  const resultEl = document.getElementById("result");

  const state = { tokens: {}, token: null, cleared: false, busy: false };

  async function loadTokens() {
    try {
      const res = await fetch("/api/tokens");
      if (!res.ok) throw new Error("HTTP " + res.status);
      const tokens = await res.json();
      gridEl.innerHTML = "";
      tokens.forEach((t, i) => {
        state.tokens[t.symbol] = t;
        const card = document.createElement("button");
        card.type = "button";
        card.className = "token-card" + (i === 0 ? " selected" : "");
        card.dataset.sym = t.symbol;
        card.innerHTML =
          '<div class="sym">' + escapeHtml(t.symbol) + "</div>" +
          '<div class="nm">' + escapeHtml(t.name) + "</div>" +
          '<div class="dec">' + t.decimals + " decimals</div>";
        card.addEventListener("click", () => selectToken(t.symbol));
        gridEl.appendChild(card);
      });
      if (tokens[0]) selectToken(tokens[0].symbol);
    } catch (e) {
      gridEl.innerHTML = '<div class="token-loading">Could not load tokens: ' + escapeHtml(e.message || String(e)) + "</div>";
    }
  }

  function selectToken(sym) {
    state.token = sym;
    for (const c of gridEl.children) c.classList && c.classList.toggle("selected", c.dataset.sym === sym);
    const t = state.tokens[sym];
    if (t) {
      const one = Math.pow(10, t.decimals).toLocaleString("en-US");
      hintEl.textContent = "Amount in base units · " + t.symbol + " has " + t.decimals + " decimals (1 " + t.symbol + " = " + one + ").";
    }
    refresh();
  }

  function refresh() {
    const ok = state.cleared && !state.busy && !!state.token && addrEl.value.trim().length > 0 && Number(amtEl.value) > 0;
    pubBtn.disabled = !ok;
    privBtn.disabled = !ok;
  }

  document.addEventListener("faucet:cleared", () => {
    state.cleared = true;
    if (statusEl) { statusEl.textContent = "— cleared, minting unlocked"; statusEl.style.color = "#ff5500"; }
    refresh();
  });

  [addrEl, amtEl].forEach((el) => el.addEventListener("input", refresh));

  async function mint(noteType) {
    if (state.busy || pubBtn.disabled) return;
    state.busy = true;
    refresh();
    showHead("pending", "Minting " + state.token + "…");
    showBody("Submitting the transaction to the network. This can take a few seconds.");
    try {
      const res = await fetch("/api/mint", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          token: state.token,
          address: addrEl.value.trim(),
          amount: Math.trunc(Number(amtEl.value)),
          note_type: noteType,
        }),
      });
      const text = await res.text();
      if (!res.ok) {
        showHead("err", "Mint failed");
        showBody(escapeHtml(text), true);
      } else {
        renderSuccess(noteType, JSON.parse(text));
      }
    } catch (e) {
      showHead("err", "Request error");
      showBody(escapeHtml(e.message || String(e)), true);
    } finally {
      state.busy = false;
      refresh();
    }
  }

  pubBtn.addEventListener("click", () => mint("public"));
  privBtn.addEventListener("click", () => mint("private"));

  function renderSuccess(noteType, out) {
    const label = noteType === "private" ? "Private" : "Public";
    showHead("ok", '<span class="highlight">' + label + '</span> note minted!');
    let body =
      '<div class="result-row"><b>Tx</b><span class="mono">' + escapeHtml(out.tx_id) + "</span></div>" +
      '<div class="result-row"><b>Note</b><span class="mono">' + escapeHtml(out.note_id) + "</span></div>";
    if (out.note_b64) {
      const url = noteDownloadUrl(out.note_b64);
      body +=
        '<a class="dl-button" download="note_' + out.note_id.slice(2, 12) + '.mno" href="' + url + '">' +
        '<svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M11 5a1 1 0 1 1 2 0v7.16l3.24-3.24 1.42 1.41L12 16 6.34 10.33l1.42-1.41L11 12.16V5Z"/><path d="M4 14h2v4h12v-4h2v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2v-4Z"/></svg>' +
        "Download note</a>" +
        '<div class="result-row" style="margin-top:8px;color:rgba(0,0,0,.5)">Import this file into your wallet to claim.</div>';
    } else {
      body += '<div class="result-row" style="color:rgba(0,0,0,.5)">Go to your wallet — the note appears on its next sync.</div>';
    }
    showBody(body, true);
  }

  function noteDownloadUrl(b64) {
    const bin = atob(b64);
    const bytes = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return URL.createObjectURL(new Blob([bytes], { type: "application/octet-stream" }));
  }

  function showHead(kind, html) {
    resultEl.hidden = false;
    resultEl.className = "result " + kind;
    resultEl.innerHTML = '<div class="result-head">' + html + '</div><div class="result-body" id="result-body"></div>';
  }
  function showBody(content, isHtml) {
    const b = document.getElementById("result-body");
    if (!b) return;
    if (isHtml) b.innerHTML = content;
    else b.textContent = content;
  }

  function escapeHtml(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  loadTokens();
  refresh();
})();
