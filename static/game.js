// Space-themed endless runner: a jumping astronaut hurdling asteroids. Clearing
// a score of 100 unlocks the mint buttons (dispatches `faucet:cleared` once).
// Original art drawn with canvas primitives — no external assets.
(function () {
  const canvas = document.getElementById("game");
  if (!canvas) return;
  const ctx = canvas.getContext("2d");
  const W = canvas.width;
  const H = canvas.height;
  const SURFACE = H - 26;
  const GOAL = 100;
  const ACCENT = "#ff5500";

  const overlay = document.getElementById("gameOverlay") || document.getElementById("game-overlay");
  const startBtn = document.getElementById("start-btn");
  const scoreEl = document.getElementById("score");
  const goalEl = document.getElementById("goal");

  // starfield (static parallax dots)
  const stars = [];
  for (let i = 0; i < 60; i++) {
    stars.push({
      x: ((i * 9301 + 49297) % 233280) / 233280 * W,
      y: ((i * 49297 + 233) % 9973) / 9973 * (SURFACE - 12),
      r: (i % 3 === 0) ? 1.4 : 0.8,
      sp: (i % 3 === 0) ? 0.6 : 0.3,
    });
  }

  let state = "idle"; // idle | running | over
  let astro, rocks, speed, score, spawnIn, raf, cleared, clearedFired, t;

  function reset() {
    astro = { x: 70, y: SURFACE, vy: 0, h: 30, w: 20, onGround: true };
    rocks = [];
    speed = 4.2;
    score = 0;
    spawnIn = 55;
    cleared = false;
    clearedFired = false;
    t = 0;
    updateHud();
  }

  function updateHud() {
    if (scoreEl) scoreEl.textContent = "Score " + Math.floor(score);
    if (goalEl) {
      goalEl.textContent = cleared ? "Cleared! 100/100" : "Goal " + GOAL;
      goalEl.classList.toggle("cleared", cleared);
    }
  }

  function jump() {
    if (state !== "running") { start(); return; }
    if (astro.onGround) { astro.vy = -10.2; astro.onGround = false; }
  }

  function start() {
    reset();
    state = "running";
    if (overlay) overlay.classList.add("hidden");
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(loop);
  }

  function gameOver() {
    state = "over";
    if (startBtn) {
      startBtn.textContent = cleared
        ? "Score " + Math.floor(score) + " — tap to play again"
        : "Reached " + Math.floor(score) + " / " + GOAL + " — tap to retry";
    }
    if (overlay) overlay.classList.remove("hidden");
  }

  function spawn() {
    const big = score > 40 && (t % 5 === 0);
    rocks.push({ x: W + 12, r: big ? 16 : 9 + (t % 7) });
  }

  function loop() {
    t += 1;
    astro.vy += 0.6;
    astro.y += astro.vy;
    if (astro.y >= SURFACE) { astro.y = SURFACE; astro.vy = 0; astro.onGround = true; }

    spawnIn -= 1;
    if (spawnIn <= 0) {
      spawn();
      spawnIn = Math.max(34, 92 - Math.floor(score / 6)) + (t % 22);
    }

    speed = 4.2 + score / 38;
    for (const r of rocks) r.x -= speed;
    rocks = rocks.filter((r) => r.x + r.r > -12);

    score += 0.2 + speed * 0.02;
    if (!cleared && score >= GOAL) {
      cleared = true;
      if (!clearedFired) {
        clearedFired = true;
        document.dispatchEvent(new CustomEvent("faucet:cleared", { detail: { score: GOAL } }));
      }
    }

    // collision: astronaut AABB vs rock circle center on the surface
    const ax = astro.x - astro.w / 2;
    const ay = astro.y - astro.h;
    for (const r of rocks) {
      const cx = r.x;
      const cy = SURFACE - r.r;
      const nx = Math.max(ax, Math.min(cx, ax + astro.w));
      const ny = Math.max(ay, Math.min(cy, ay + astro.h));
      if ((cx - nx) ** 2 + (cy - ny) ** 2 < (r.r - 1) ** 2) {
        draw();
        gameOver();
        return;
      }
    }

    updateHud();
    draw();
    if (state === "running") raf = requestAnimationFrame(loop);
  }

  function draw() {
    // space
    ctx.fillStyle = "#0a0c18";
    ctx.fillRect(0, 0, W, H);
    // stars
    for (const s of stars) {
      const sx = (s.x - score * s.sp * 6) % W;
      ctx.globalAlpha = 0.5 + 0.5 * Math.abs(Math.sin((t + s.y) * 0.02 * s.sp + s.x));
      ctx.fillStyle = "#cfe0ff";
      ctx.fillRect((sx + W) % W, s.y, s.r, s.r);
    }
    ctx.globalAlpha = 1;

    // lunar surface
    ctx.fillStyle = "#141a30";
    ctx.fillRect(0, SURFACE + 2, W, H - SURFACE);
    ctx.strokeStyle = "#2a3458";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.moveTo(0, SURFACE + 2);
    ctx.lineTo(W, SURFACE + 2);
    ctx.stroke();
    ctx.fillStyle = "#202a4a";
    const off = Math.floor((score * 6) % 46);
    for (let x = -off; x < W; x += 46) ctx.fillRect(x, SURFACE + 9, 18, 2);

    // flaming meteors (fire trails behind, since they streak leftward)
    for (const r of rocks) {
      const cy = SURFACE - r.r;
      drawMeteorFire(r.x, cy, r.r);
    }
    for (const r of rocks) {
      const cy = SURFACE - r.r;
      // rock body
      ctx.fillStyle = "#737b8c";
      ctx.beginPath();
      ctx.arc(r.x, cy, r.r, 0, Math.PI * 2);
      ctx.fill();
      // craters
      ctx.fillStyle = "#565d6e";
      ctx.beginPath();
      ctx.arc(r.x - r.r * 0.3, cy - r.r * 0.2, r.r * 0.28, 0, Math.PI * 2);
      ctx.arc(r.x + r.r * 0.35, cy + r.r * 0.25, r.r * 0.18, 0, Math.PI * 2);
      ctx.fill();
      // hot leading rim (the side cutting through space)
      ctx.strokeStyle = "rgba(255,120,40,0.85)";
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.arc(r.x, cy, r.r - 0.5, Math.PI * 0.62, Math.PI * 1.38);
      ctx.stroke();
    }

    drawAstronaut(astro.x, astro.y, astro.onGround);
  }

  // A flickering teardrop flame trailing to the +x side of a meteor.
  function drawMeteorFire(x, cy, r) {
    const flick = 1 + Math.sin(t * 0.6 + x * 0.7) * 0.18;
    const len = r * 2.6 * flick;
    flame(x, cy, r * 1.15, len, "rgba(255,80,0,0.30)");
    flame(x, cy, r * 0.8, len * 0.74, "rgba(255,140,10,0.55)");
    flame(x, cy, r * 0.46, len * 0.46, "rgba(255,212,70,0.9)");
    // a couple of trailing sparks
    ctx.fillStyle = "rgba(255,180,60,0.9)";
    const sx = x + len * (0.7 + 0.25 * Math.sin(t * 0.9 + x));
    ctx.fillRect(sx, cy - 1 + Math.sin(t * 0.7 + x) * 2, 1.6, 1.6);
  }

  function flame(x, cy, half, len, color) {
    const wobble = Math.sin(t * 0.7 + x) * 1.5;
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.moveTo(x, cy - half);
    ctx.quadraticCurveTo(x + len * 0.5, cy - half * 0.25, x + len, cy + wobble);
    ctx.quadraticCurveTo(x + len * 0.5, cy + half * 0.25, x, cy + half);
    ctx.closePath();
    ctx.fill();
  }

  function drawAstronaut(cx, footY, onGround) {
    const w = astro.w, h = astro.h;
    const topY = footY - h;
    const bodyW = w, bodyH = h * 0.46;
    const bodyY = topY + h * 0.34;
    const swing = onGround ? Math.sin(t * 0.45) : -0.4;

    // jetpack thrust when airborne
    if (!onGround) {
      const jl = 7 + Math.abs(Math.sin(t * 0.8)) * 5;
      vflame(cx - bodyW / 2 - 2, bodyY + bodyH - 1, 5, jl, "rgba(255,80,0,0.35)");
      vflame(cx - bodyW / 2 - 2, bodyY + bodyH - 1, 3, jl * 0.7, "rgba(255,200,60,0.9)");
    }

    // backpack (life-support)
    ctx.fillStyle = "#b9c0cf";
    roundRect(cx - bodyW / 2 - 5, bodyY, 6, bodyH - 1, 2);
    ctx.fill();
    ctx.fillStyle = ACCENT;
    ctx.fillRect(cx - bodyW / 2 - 4, bodyY + 2, 4, 2);

    // legs + boots (running cycle grounded, tucked airborne)
    const legTop = bodyY + bodyH - 2;
    if (onGround) {
      leg(cx - 6, legTop, 8 + swing * 4);
      leg(cx + 2, legTop, 8 - swing * 4);
    } else {
      leg(cx - 6, legTop, 5);
      leg(cx + 2, legTop, 4);
    }

    // body suit + soft lower shading
    ctx.fillStyle = "#f4f6fc";
    roundRect(cx - bodyW / 2, bodyY, bodyW, bodyH, 6);
    ctx.fill();
    ctx.fillStyle = "#e2e7f1";
    roundRect(cx - bodyW / 2, bodyY + bodyH * 0.55, bodyW, bodyH * 0.45, 6);
    ctx.fill();
    // chest control panel
    ctx.fillStyle = ACCENT;
    ctx.fillRect(cx - 4, bodyY + 5, 8, 4);
    ctx.fillStyle = "#ffe0c2";
    ctx.fillRect(cx - 3, bodyY + 6, 2, 2);

    // forward arm + glove (swings while running)
    const armY = bodyY + 4 + swing * 1.5;
    ctx.fillStyle = "#eef1f7";
    roundRect(cx + bodyW / 2 - 3, armY, 9, 5, 2);
    ctx.fill();
    ctx.fillStyle = ACCENT;
    ctx.beginPath();
    ctx.arc(cx + bodyW / 2 + 6, armY + 2.5, 2.4, 0, Math.PI * 2);
    ctx.fill();

    // collar
    ctx.fillStyle = "#d6dbe6";
    ctx.fillRect(cx - bodyW * 0.34, bodyY - 2, bodyW * 0.68, 3);

    // helmet
    const headR = h * 0.27;
    const headCY = topY + headR + 1;
    ctx.fillStyle = "#f8faff";
    ctx.beginPath();
    ctx.arc(cx, headCY, headR, 0, Math.PI * 2);
    ctx.fill();
    // visor
    ctx.fillStyle = "#101f3a";
    ctx.beginPath();
    ctx.arc(cx + 1, headCY + 1, headR * 0.6, 0, Math.PI * 2);
    ctx.fill();
    // visor glints (orange + cyan)
    ctx.fillStyle = "#ff7a33";
    ctx.beginPath();
    ctx.arc(cx - 1, headCY + 2, headR * 0.16, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "#bfe0ff";
    ctx.beginPath();
    ctx.arc(cx + 3.5, headCY - 2, headR * 0.16, 0, Math.PI * 2);
    ctx.fill();
    // antenna
    ctx.strokeStyle = "#cfd6e3";
    ctx.lineWidth = 1.4;
    ctx.beginPath();
    ctx.moveTo(cx + headR * 0.55, headCY - headR * 0.7);
    ctx.lineTo(cx + headR * 0.95, headCY - headR * 1.35);
    ctx.stroke();
    ctx.fillStyle = ACCENT;
    ctx.beginPath();
    ctx.arc(cx + headR * 0.95, headCY - headR * 1.35, 1.6, 0, Math.PI * 2);
    ctx.fill();
  }

  function leg(x, top, len) {
    ctx.fillStyle = "#e7ebf3";
    ctx.fillRect(x, top, 5, Math.max(3, len - 3));
    ctx.fillStyle = "#aab2c4"; // boot
    ctx.fillRect(x - 1, top + Math.max(3, len - 3), 6, 3);
  }

  // vertical (downward) flame, used for the jetpack
  function vflame(x, y, half, len, color) {
    const wob = Math.sin(t * 0.8 + x) * 1.2;
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.moveTo(x - half, y);
    ctx.quadraticCurveTo(x - half * 0.25, y + len * 0.5, x + wob, y + len);
    ctx.quadraticCurveTo(x + half * 0.25, y + len * 0.5, x + half, y);
    ctx.closePath();
    ctx.fill();
  }

  function roundRect(x, y, w, h, r) {
    ctx.beginPath();
    ctx.moveTo(x + r, y);
    ctx.arcTo(x + w, y, x + w, y + h, r);
    ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r);
    ctx.arcTo(x, y, x + w, y, r);
    ctx.closePath();
  }

  window.addEventListener("keydown", (e) => {
    if (e.code === "Space" || e.code === "ArrowUp") { e.preventDefault(); jump(); }
  });
  canvas.addEventListener("pointerdown", (e) => { e.preventDefault(); jump(); });
  if (startBtn) startBtn.addEventListener("click", (e) => { e.preventDefault(); start(); });

  reset();
  draw();
})();
