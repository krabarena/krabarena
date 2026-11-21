// KRAB INVADERS — a self-contained Space Invaders easter egg.
//
// You are the krab cannon at the bottom of the screen; the descending grid is
// made of AI-lab and web-search-API emblems. Clear a wave, the next drops
// lower and marches faster. Lose three krabs → GAME OVER. Clear every wave →
// victory.
//
// Fully client-side: no server route, no template, no external asset or
// network call. The whole thing mounts as a fixed <canvas> overlay created in
// JS and is triggered by the Konami code or by clicking the hero invader
// cluster. Rendering is 100% canvas 2D (no injected SVG). Styling for the thin
// DOM chrome (overlay, close + touch buttons) lives in
// styles/components/invaders.css.

const KONAMI = [
  "ArrowUp",
  "ArrowUp",
  "ArrowDown",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "ArrowLeft",
  "ArrowRight",
  "b",
  "a"
];

const HI_SCORE_KEY = "krab-invaders-hi";

// Logical play-field resolution. Everything is drawn in these units and the
// backing store is scaled by devicePixelRatio for crisp pixels; the element is
// then fit into the viewport with CSS while preserving aspect ratio.
const VIEW_W = 480;
const VIEW_H = 600;
const HUD_H = 40; // top strip reserved for SCORE / WAVE / LIVES

// Invader roster: [label, tileColor, darkText]. A short pixel monogram on a
// brand-coloured tile — recognisable, funny, and fully self-contained (no
// logo files to load or break). Mix of AI labs + web/search APIs.
const ROSTER = [
  ["GPT", "#10a37f", false],
  ["CLAUDE", "#d97757", false],
  ["GEMINI", "#4285f4", false],
  ["LLAMA", "#0866ff", false],
  ["MISTRAL", "#fa5310", false],
  ["GROK", "#e8e8e8", true],
  ["DEEPSEEK", "#4d6bfe", false],
  ["QWEN", "#615ced", false],
  ["COHERE", "#39594d", false],
  ["COPILOT", "#0078d4", false],
  ["TAVILY", "#4b8bf5", false],
  ["BRAVE", "#fb542b", false],
  ["EXA", "#1f6feb", false],
  ["SERPER", "#10b981", false],
  ["PPLX", "#20808d", false],
  ["FIRECRAWL", "#f97316", false],
  ["JINA", "#ec2d6f", false],
  ["BING", "#0c8484", false]
];

const COLS = 8;
const ROWS = 5;
const TOTAL_WAVES = 4;

// Palette pulled from the arcade tokens once at boot (canvas can't read CSS
// vars directly), with hard fallbacks so the game still renders if the tokens
// ever move.
function readPalette() {
  const root = getComputedStyle(document.documentElement);
  const pick = (name, fallback) => {
    const value = root.getPropertyValue(name).trim();
    return value || fallback;
  };
  return {
    bg: pick("--ka-bg", "#05070d"),
    surface: pick("--ka-surface", "#0b0f18"),
    line: pick("--ka-line", "#16324a"),
    green: pick("--ka-green", "#3dff7c"),
    orange: pick("--ka-orange", "#f06827"),
    orangeHover: pick("--ka-orange-hover", "#ff8a4d"),
    cyan: pick("--ka-cyan", "#39d3ff"),
    gold: pick("--ka-gold", "#ffd23d"),
    red: pick("--ka-red", "#ff3355"),
    text: pick("--ka-text", "#eafff1"),
    text3: pick("--ka-text-3", "#9fd8b4"),
    text4: pick("--ka-text-4", "#7da48b")
  };
}

let controller = null;

// Public entry point wired into main.js — installs the launch triggers once.
export function initInvaders() {
  if (window.__krabInvadersInstalled) return;
  window.__krabInvadersInstalled = true;

  installKonami();
  installHeroTrigger();
  installSecretPixel();

  // Direct launcher for manual / automated verification from the console.
  window.__krabInvaders = launch;
}

function launch() {
  if (!controller) controller = new KrabInvaders();
  controller.open();
}

// ── Triggers ────────────────────────────────────────────────────────────────

function installKonami() {
  let index = 0;
  window.addEventListener("keydown", (event) => {
    // Don't hijack typing.
    const el = document.activeElement;
    if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable)) {
      return;
    }
    const key = event.key.length === 1 ? event.key.toLowerCase() : event.key;
    if (key === KONAMI[index]) {
      index += 1;
      if (index === KONAMI.length) {
        index = 0;
        launch();
      }
    } else {
      // Allow a mismatched key to still start a fresh run of the sequence.
      index = key === KONAMI[0] ? 1 : 0;
    }
  });
}

function installHeroTrigger() {
  const cluster = document.querySelector(".ka-invaders");
  if (!cluster) return;
  // Decorative cluster stays aria-hidden; mouse users discover the game by
  // clicking it, keyboard users use the Konami code. A CSS rule gives it a
  // pointer cursor + title as the affordance.
  cluster.classList.add("ka-invaders--playable");
  cluster.setAttribute("title", "▶ KRAB INVADERS");
  cluster.addEventListener("click", launch);
}

// A single dim pixel at the very top-centre of every page. Barely visible
// until you hover it (then it glows) — the site-wide "secret" way in, so the
// game is reachable without the hero cluster or the Konami code.
function installSecretPixel() {
  if (document.querySelector(".krab-secret-pixel")) return;
  const pixel = document.createElement("button");
  pixel.type = "button";
  pixel.className = "krab-secret-pixel";
  pixel.setAttribute("aria-label", "Play Krab Invaders");
  pixel.title = "▶ KRAB INVADERS";
  pixel.addEventListener("click", launch);
  document.body.appendChild(pixel);
}

// ── Game controller ──────────────────────────────────────────────────────────

class KrabInvaders {
  constructor() {
    this.palette = readPalette();
    this.reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    this.state = "title"; // title | playing | gameover | victory
    this.raf = 0;
    this.lastTs = 0;
    this.isOpen = false;
    this.sound = new Blipper();

    this.score = 0;
    this.hi = readHiScore();
    this.lives = 3;
    this.wave = 1;

    this.keys = new Set();
    this.touchMoveX = null; // target x from touch drag, or null
    this.starfield = makeStarfield(this.reducedMotion ? 24 : 60);

    this.buildDom();
    this.resetEntities();
    this.bindInput();
  }

  buildDom() {
    const overlay = document.createElement("div");
    overlay.className = "krab-invaders";
    overlay.setAttribute("role", "dialog");
    overlay.setAttribute("aria-modal", "true");
    overlay.setAttribute("aria-label", "Krab Invaders mini-game");
    overlay.hidden = true;

    const stage = document.createElement("div");
    stage.className = "ki-stage";

    const canvas = document.createElement("canvas");
    canvas.className = "ki-canvas";
    stage.appendChild(canvas);

    const close = document.createElement("button");
    close.type = "button";
    close.className = "ki-close";
    close.setAttribute("aria-label", "Quit game (Esc)");
    close.textContent = "✕";
    stage.appendChild(close);

    // Touch controls (shown only on coarse pointers via CSS).
    const pad = document.createElement("div");
    pad.className = "ki-pad";
    const left = padButton("◄", "Move left");
    const right = padButton("►", "Move right");
    const fire = padButton("▲", "Fire");
    fire.classList.add("ki-pad-fire");
    pad.append(left, right, fire);
    stage.appendChild(pad);

    overlay.appendChild(stage);
    document.body.appendChild(overlay);

    this.overlay = overlay;
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.closeBtn = close;
    this.padLeft = left;
    this.padRight = right;
    this.padFire = fire;
  }

  resetEntities() {
    this.playerW = 44;
    this.playerH = 26;
    this.player = { x: VIEW_W / 2 - this.playerW / 2, y: VIEW_H - this.playerH - 14 };
    this.bullets = [];
    this.bombs = [];
    this.particles = [];
    this.fireCooldown = 0;
    this.spawnWave(this.wave);
  }

  spawnWave(wave) {
    const marginX = 32;
    const gridW = VIEW_W - marginX * 2;
    const cellW = gridW / COLS;
    const cellH = 46;
    const tileW = 46;
    const tileH = 40;
    const top = HUD_H + 18 + (wave - 1) * 16;
    this.invaders = [];
    for (let r = 0; r < ROWS; r += 1) {
      for (let c = 0; c < COLS; c += 1) {
        const brand = ROSTER[(r * COLS + c + (wave - 1) * 3) % ROSTER.length];
        this.invaders.push({
          x: marginX + c * cellW + (cellW - tileW) / 2,
          y: top + r * cellH,
          w: tileW,
          h: tileH,
          label: brand[0],
          color: brand[1],
          darkText: brand[2],
          points: (ROWS - r) * 10,
          alive: true
        });
      }
    }
    this.invDir = 1; // 1 = right, -1 = left
    this.invSpeed = 18 + (wave - 1) * 8; // logical px/sec base
    this.invStepDown = 16;
    this.bombTimer = 0;
    this.animPhase = 0;
  }

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  open() {
    if (this.isOpen) return;
    this.isOpen = true;
    this.prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    this.overlay.hidden = false;
    this.lastFocus = document.activeElement;
    this.resize();
    if (this.state !== "playing") this.state = "title";
    this.closeBtn.focus();
    this.lastTs = 0;
    this.loop = this.loop.bind(this);
    this.raf = window.requestAnimationFrame(this.loop);
  }

  close() {
    if (!this.isOpen) return;
    this.isOpen = false;
    window.cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.overlay.hidden = true;
    document.body.style.overflow = this.prevOverflow || "";
    this.keys.clear();
    this.touchMoveX = null;
    if (this.lastFocus && typeof this.lastFocus.focus === "function") this.lastFocus.focus();
  }

  startRun() {
    this.score = 0;
    this.lives = 3;
    this.wave = 1;
    this.resetEntities();
    this.state = "playing";
    this.sound.blip(660, 0.06);
  }

  // ── Input ────────────────────────────────────────────────────────────────

  bindInput() {
    this.onKeyDown = (event) => {
      if (!this.isOpen) return;
      const key = event.key;
      if (key === "Escape") {
        event.preventDefault();
        this.close();
        return;
      }
      if (key === "Tab") {
        // Minimal focus trap: keep focus on the close button.
        event.preventDefault();
        this.closeBtn.focus();
        return;
      }
      if (key === "m" || key === "M") {
        this.sound.toggle();
        return;
      }
      if (
        key === " " ||
        key === "ArrowLeft" ||
        key === "ArrowRight" ||
        key === "ArrowUp" ||
        key === "ArrowDown"
      ) {
        event.preventDefault();
      }
      if (key === " " || key === "Enter") {
        if (this.state === "title") this.startRun();
        else if (this.state === "gameover" || this.state === "victory") this.startRun();
      }
      this.keys.add(normalizeKey(key));
    };
    this.onKeyUp = (event) => {
      this.keys.delete(normalizeKey(event.key));
    };
    window.addEventListener("keydown", this.onKeyDown);
    window.addEventListener("keyup", this.onKeyUp);

    this.closeBtn.addEventListener("click", () => this.close());

    // Touch buttons.
    holdButton(
      this.padLeft,
      () => this.keys.add("left"),
      () => this.keys.delete("left")
    );
    holdButton(
      this.padRight,
      () => this.keys.add("right"),
      () => this.keys.delete("right")
    );
    holdButton(
      this.padFire,
      () => this.keys.add("fire"),
      () => this.keys.delete("fire")
    );

    // Drag anywhere on the canvas to move; tap to start / fire.
    this.canvas.addEventListener("pointerdown", (event) => this.onPointer(event, true));
    this.canvas.addEventListener("pointermove", (event) => this.onPointer(event, false));
    this.canvas.addEventListener("pointerup", () => {
      this.touchMoveX = null;
    });
    this.canvas.addEventListener("pointercancel", () => {
      this.touchMoveX = null;
    });

    window.addEventListener("resize", () => {
      if (this.isOpen) this.resize();
    });
    window.addEventListener("blur", () => this.keys.clear());
  }

  onPointer(event, isDown) {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((event.clientX - rect.left) / rect.width) * VIEW_W;
    if (isDown) {
      if (this.state === "title" || this.state === "gameover" || this.state === "victory") {
        this.startRun();
        return;
      }
      this.keys.add("fire");
      window.setTimeout(() => this.keys.delete("fire"), 90);
    }
    if (this.state === "playing" && event.pointerType === "touch") {
      this.touchMoveX = x;
    }
  }

  // ── Sizing ────────────────────────────────────────────────────────────────

  resize() {
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    const maxW = Math.min(window.innerWidth - 24, 520);
    const maxH = window.innerHeight - 24;
    const scale = Math.min(maxW / VIEW_W, maxH / VIEW_H);
    const cssW = Math.floor(VIEW_W * scale);
    const cssH = Math.floor(VIEW_H * scale);
    this.canvas.style.width = `${cssW}px`;
    this.canvas.style.height = `${cssH}px`;
    this.canvas.width = Math.floor(VIEW_W * dpr);
    this.canvas.height = Math.floor(VIEW_H * dpr);
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.ctx.imageSmoothingEnabled = false;
  }

  // ── Main loop ──────────────────────────────────────────────────────────────

  loop(ts) {
    if (!this.isOpen) return;
    if (!this.lastTs) this.lastTs = ts;
    let dt = (ts - this.lastTs) / 1000;
    this.lastTs = ts;
    if (dt > 0.05) dt = 0.05; // clamp after tab-switch stalls
    if (this.state === "playing") this.update(dt);
    this.updateStarfield(dt);
    this.render();
    this.raf = window.requestAnimationFrame(this.loop);
  }

  update(dt) {
    this.animPhase += dt;

    // Player movement.
    const speed = 220;
    let vx = 0;
    if (this.keys.has("left")) vx -= 1;
    if (this.keys.has("right")) vx += 1;
    this.player.x += vx * speed * dt;
    if (this.touchMoveX !== null) {
      const target = this.touchMoveX - this.playerW / 2;
      this.player.x += (target - this.player.x) * Math.min(1, dt * 14);
    }
    this.player.x = clamp(this.player.x, 6, VIEW_W - this.playerW - 6);

    // Firing.
    this.fireCooldown -= dt;
    if (this.keys.has("fire") && this.fireCooldown <= 0) {
      this.bullets.push({
        x: this.player.x + this.playerW / 2 - 2,
        y: this.player.y - 6,
        w: 4,
        h: 12
      });
      this.fireCooldown = 0.34;
      this.sound.blip(880, 0.04);
    }

    // Bullets.
    for (const b of this.bullets) b.y -= 460 * dt;
    this.bullets = this.bullets.filter((b) => b.y + b.h > HUD_H);

    // Invader march. Speed scales up as the swarm thins (classic feel).
    const alive = this.invaders.filter((i) => i.alive);
    if (alive.length === 0) {
      this.nextWave();
      return;
    }
    const thinFactor = 1 + ((ROWS * COLS - alive.length) / (ROWS * COLS)) * 2.2;
    const step = this.invSpeed * thinFactor * dt * this.invDir;
    let minX = Infinity;
    let maxX = -Infinity;
    for (const inv of alive) {
      minX = Math.min(minX, inv.x);
      maxX = Math.max(maxX, inv.x + inv.w);
    }
    let dropped = false;
    if ((this.invDir > 0 && maxX + step >= VIEW_W - 8) || (this.invDir < 0 && minX + step <= 8)) {
      this.invDir *= -1;
      dropped = true;
    }
    for (const inv of alive) {
      if (dropped) inv.y += this.invStepDown;
      else inv.x += step;
    }

    // Invader bombs.
    this.bombTimer -= dt;
    if (this.bombTimer <= 0) {
      this.bombTimer = Math.max(0.35, 1.4 - this.wave * 0.15);
      const shooter = alive[Math.floor(rand() * alive.length)];
      this.bombs.push({ x: shooter.x + shooter.w / 2 - 2, y: shooter.y + shooter.h, w: 4, h: 12 });
    }
    for (const bomb of this.bombs) bomb.y += (150 + this.wave * 18) * dt;
    this.bombs = this.bombs.filter((b) => b.y < VIEW_H);

    this.handleCollisions(alive);

    // Invaders reached the krab line → lose the run.
    for (const inv of alive) {
      if (inv.y + inv.h >= this.player.y) {
        this.loseLife(true);
        return;
      }
    }

    for (const p of this.particles) {
      p.x += p.vx * dt;
      p.y += p.vy * dt;
      p.life -= dt;
    }
    this.particles = this.particles.filter((p) => p.life > 0);
  }

  handleCollisions(alive) {
    // Player bullets vs invaders.
    for (const b of this.bullets) {
      for (const inv of alive) {
        if (inv.alive && overlaps(b, inv)) {
          inv.alive = false;
          b.y = -999;
          this.score += inv.points;
          this.spawnExplosion(inv.x + inv.w / 2, inv.y + inv.h / 2, inv.color);
          this.sound.blip(200, 0.08, "square");
          break;
        }
      }
    }
    this.bullets = this.bullets.filter((b) => b.y > -100);

    // Bombs vs krab.
    for (const bomb of this.bombs) {
      const hit = {
        x: this.player.x,
        y: this.player.y,
        w: this.playerW,
        h: this.playerH
      };
      if (overlaps(bomb, hit)) {
        bomb.y = VIEW_H + 999;
        this.loseLife(false);
        return;
      }
    }
    this.bombs = this.bombs.filter((b) => b.y < VIEW_H);
  }

  loseLife(overrun) {
    this.lives -= 1;
    this.spawnExplosion(
      this.player.x + this.playerW / 2,
      this.player.y + this.playerH / 2,
      this.palette.orange
    );
    this.sound.blip(120, 0.2, "sawtooth");
    if (this.lives <= 0) {
      this.endRun();
      return;
    }
    // Reset positions for another life; a full overrun also rebuilds the wave.
    this.bombs = [];
    this.bullets = [];
    this.player.x = VIEW_W / 2 - this.playerW / 2;
    if (overrun) this.spawnWave(this.wave);
  }

  nextWave() {
    if (this.wave >= TOTAL_WAVES) {
      this.state = "victory";
      this.commitHi();
      this.sound.win();
      return;
    }
    this.wave += 1;
    this.score += 100; // wave-clear bonus
    this.bombs = [];
    this.bullets = [];
    this.spawnWave(this.wave);
    this.player.x = VIEW_W / 2 - this.playerW / 2;
    this.sound.blip(520, 0.08);
  }

  endRun() {
    this.state = "gameover";
    this.commitHi();
  }

  commitHi() {
    if (this.score > this.hi) {
      this.hi = this.score;
      writeHiScore(this.hi);
    }
  }

  spawnExplosion(x, y, color) {
    const count = this.reducedMotion ? 5 : 12;
    for (let i = 0; i < count; i += 1) {
      const a = rand() * Math.PI * 2;
      const sp = 40 + rand() * 120;
      this.particles.push({
        x,
        y,
        vx: Math.cos(a) * sp,
        vy: Math.sin(a) * sp,
        life: 0.3 + rand() * 0.3,
        color
      });
    }
  }

  updateStarfield(dt) {
    if (this.reducedMotion) return;
    for (const s of this.starfield) {
      s.y += s.speed * dt;
      if (s.y > VIEW_H) {
        s.y = 0;
        s.x = rand() * VIEW_W;
      }
    }
  }

  // ── Rendering ──────────────────────────────────────────────────────────────

  render() {
    const ctx = this.ctx;
    const p = this.palette;
    ctx.fillStyle = p.bg;
    ctx.fillRect(0, 0, VIEW_W, VIEW_H);
    this.drawStarfield();

    if (this.state === "playing" || this.state === "gameover") {
      this.drawInvaders();
      this.drawBullets();
      this.drawParticles();
      if (this.lives > 0 || this.state === "playing")
        this.drawKrab(this.player.x, this.player.y, this.playerW, this.playerH);
    }
    this.drawHud();

    if (this.state === "title") this.drawTitle();
    if (this.state === "gameover")
      this.drawCenterCard("GAME OVER", `SCORE ${this.score}`, "PRESS SPACE / TAP TO RETRY", p.red);
    if (this.state === "victory")
      this.drawCenterCard(
        "ALL LABS DEFEATED",
        `SCORE ${this.score}`,
        "PRESS SPACE / TAP TO PLAY AGAIN",
        p.gold
      );
  }

  drawStarfield() {
    const ctx = this.ctx;
    ctx.fillStyle = this.palette.text4;
    for (const s of this.starfield) {
      ctx.globalAlpha = s.a;
      ctx.fillRect(Math.round(s.x), Math.round(s.y), s.size, s.size);
    }
    ctx.globalAlpha = 1;
  }

  drawHud() {
    const ctx = this.ctx;
    const p = this.palette;
    ctx.fillStyle = p.surface;
    ctx.fillRect(0, 0, VIEW_W, HUD_H);
    ctx.fillStyle = p.line;
    ctx.fillRect(0, HUD_H - 2, VIEW_W, 2);

    this.pixelText(`SCORE ${this.score}`, 10, 15, 8, p.green, "left");
    this.pixelText(`HI ${this.hi}`, 10, 27, 7, p.text3, "left");
    this.pixelText(`WAVE ${this.wave}/${TOTAL_WAVES}`, VIEW_W / 2, 15, 8, p.cyan, "center");

    // Lives as tiny krabs.
    for (let i = 0; i < this.lives; i += 1) {
      this.drawKrab(VIEW_W - 26 - i * 22, 10, 18, 12, true);
    }
    if (!this.sound.enabled) this.pixelText("MUTED", VIEW_W - 10, 30, 6, p.text4, "right");
  }

  drawInvaders() {
    const ctx = this.ctx;
    const bob = this.reducedMotion ? 0 : Math.round(Math.sin(this.animPhase * 3) * 2);
    for (const inv of this.invaders) {
      if (!inv.alive) continue;
      const x = Math.round(inv.x);
      const y = Math.round(inv.y + bob);
      // Dark chip backing so a brand-coloured glyph pops.
      ctx.fillStyle = "#0b0f18";
      ctx.fillRect(x, y, inv.w, inv.h);
      // 2px brand-colour pixel frame.
      ctx.fillStyle = inv.color;
      ctx.fillRect(x, y, inv.w, 2);
      ctx.fillRect(x, y + inv.h - 2, inv.w, 2);
      ctx.fillRect(x, y, 2, inv.h);
      ctx.fillRect(x + inv.w - 2, y, 2, inv.h);
      // Stylized 8-bit brand mark (our own pixel rendition — no logo files).
      const draw = BRAND_SPRITES[inv.label] || drawDefaultSprite;
      draw(ctx, x + inv.w / 2, y + 15, inv.color);
      // Tiny supporting label under the mark.
      const label = inv.label.length > 7 ? inv.label.slice(0, 7) : inv.label;
      this.pixelText(label, x + inv.w / 2, y + inv.h - 5, 4, inv.color, "center");
    }
  }

  drawBullets() {
    const ctx = this.ctx;
    ctx.fillStyle = this.palette.green;
    for (const b of this.bullets) ctx.fillRect(Math.round(b.x), Math.round(b.y), b.w, b.h);
    ctx.fillStyle = this.palette.orangeHover;
    for (const bomb of this.bombs) {
      const x = Math.round(bomb.x);
      const y = Math.round(bomb.y);
      ctx.fillRect(x, y, bomb.w, bomb.h);
      ctx.fillRect(x - 2, y + 4, bomb.w + 4, 3);
    }
  }

  drawParticles() {
    const ctx = this.ctx;
    for (const part of this.particles) {
      ctx.globalAlpha = Math.max(0, Math.min(1, part.life * 3));
      ctx.fillStyle = part.color;
      ctx.fillRect(Math.round(part.x), Math.round(part.y), 3, 3);
    }
    ctx.globalAlpha = 1;
  }

  // Pixel krab cannon, drawn from rectangles so it stays crisp at any scale.
  drawKrab(x, y, w, h, mono) {
    const ctx = this.ctx;
    const p = this.palette;
    const body = mono ? p.text3 : p.orange;
    const shell = mono ? p.text4 : p.orangeHover;
    const ux = w / 11; // unit
    const uy = h / 7;
    // Shell (rounded-ish body via three stacked bars).
    ctx.fillStyle = body;
    ctx.fillRect(x + ux * 2, y + uy * 2, ux * 7, uy * 4);
    ctx.fillRect(x + ux, y + uy * 3, ux * 9, uy * 3);
    // Top dome highlight.
    ctx.fillStyle = shell;
    ctx.fillRect(x + ux * 3, y + uy, ux * 5, uy * 2);
    // Claws.
    ctx.fillStyle = body;
    ctx.fillRect(x, y + uy * 2, ux * 1.6, uy * 2);
    ctx.fillRect(x + w - ux * 1.6, y + uy * 2, ux * 1.6, uy * 2);
    // Eyes.
    ctx.fillStyle = mono ? p.bg : "#05070d";
    ctx.fillRect(x + ux * 4, y + uy * 2.4, ux, uy);
    ctx.fillRect(x + ux * 6, y + uy * 2.4, ux, uy);
  }

  drawTitle() {
    const p = this.palette;
    this.dimBackdrop();
    this.pixelText("KRAB", VIEW_W / 2, VIEW_H / 2 - 70, 22, p.orange, "center");
    this.pixelText("INVADERS", VIEW_W / 2, VIEW_H / 2 - 36, 22, p.green, "center");
    this.pixelText("SHOOT THE AI LABS", VIEW_W / 2, VIEW_H / 2 + 6, 8, p.cyan, "center");
    this.pixelText("← → / A D  MOVE", VIEW_W / 2, VIEW_H / 2 + 40, 7, p.text3, "center");
    this.pixelText("SPACE  FIRE", VIEW_W / 2, VIEW_H / 2 + 58, 7, p.text3, "center");
    this.pixelText("M MUTE   ESC QUIT", VIEW_W / 2, VIEW_H / 2 + 76, 7, p.text4, "center");
    const blink = Math.floor(this.animPhase * 2) % 2 === 0;
    if (blink)
      this.pixelText(
        "PRESS SPACE / TAP TO START",
        VIEW_W / 2,
        VIEW_H / 2 + 116,
        8,
        p.gold,
        "center"
      );
  }

  drawCenterCard(title, sub, hint, color) {
    const p = this.palette;
    this.dimBackdrop();
    this.pixelText(title, VIEW_W / 2, VIEW_H / 2 - 24, 16, color, "center");
    this.pixelText(sub, VIEW_W / 2, VIEW_H / 2 + 6, 10, p.text, "center");
    if (this.score >= this.hi)
      this.pixelText("NEW HI-SCORE!", VIEW_W / 2, VIEW_H / 2 + 30, 8, p.gold, "center");
    const blink = Math.floor(this.animPhase * 2) % 2 === 0;
    if (blink) this.pixelText(hint, VIEW_W / 2, VIEW_H / 2 + 64, 7, p.text3, "center");
  }

  dimBackdrop() {
    const ctx = this.ctx;
    ctx.fillStyle = "rgba(5,7,13,0.72)";
    ctx.fillRect(0, HUD_H, VIEW_W, VIEW_H - HUD_H);
  }

  // Press Start 2P if it loaded, else a monospace fallback. Size is in px.
  pixelText(text, x, y, size, color, align) {
    const ctx = this.ctx;
    ctx.font = `${size}px "Press Start 2P", ui-monospace, monospace`;
    ctx.textAlign = align || "left";
    ctx.textBaseline = "alphabetic";
    ctx.fillStyle = color;
    ctx.fillText(text, x, y);
  }
}

// ── Small helpers ────────────────────────────────────────────────────────────

function padButton(glyph, label) {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "ki-pad-btn";
  btn.setAttribute("aria-label", label);
  btn.textContent = glyph;
  return btn;
}

function holdButton(btn, onDown, onUp) {
  const down = (event) => {
    event.preventDefault();
    onDown();
  };
  const up = (event) => {
    event.preventDefault();
    onUp();
  };
  btn.addEventListener("pointerdown", down);
  btn.addEventListener("pointerup", up);
  btn.addEventListener("pointerleave", up);
  btn.addEventListener("pointercancel", up);
}

// ── Brand sprites ────────────────────────────────────────────────────────────
//
// Each mark is our OWN stylized 8-bit pixel rendition, drawn as a small
// bitmap of canvas rects — a transformative sprite that evokes the brand's
// shape, NOT a copy of any official logo file/SVG. "X" = the brand ink; other
// letters map to extra colours for the few multi-colour marks.

function drawBitmap(ctx, grid, cx, cy, unit, colors) {
  let w = 0;
  for (const row of grid) w = Math.max(w, row.length);
  const h = grid.length;
  const ox = Math.round(cx - (w * unit) / 2);
  const oy = Math.round(cy - (h * unit) / 2);
  for (let y = 0; y < h; y += 1) {
    const row = grid[y];
    for (let x = 0; x < row.length; x += 1) {
      const ch = row[x];
      if (ch === "." || ch === " ") continue;
      ctx.fillStyle = colors[ch] || colors.X;
      ctx.fillRect(ox + x * unit, oy + y * unit, unit, unit);
    }
  }
}

// Single-colour marks (glyph shape carries the brand; ink = tile brand colour).
const GLYPHS = {
  // OpenAI — hexagonal knot / blossom ring.
  GPT: [
    "...XXX...",
    "..X...X..",
    ".X.....X.",
    "X.......X",
    "X.......X",
    "X.......X",
    ".X.....X.",
    "..X...X..",
    "...XXX..."
  ],
  // Anthropic — spoked sunburst / asterisk.
  CLAUDE: [
    "....X....",
    "X...X...X",
    ".X..X..X.",
    "..X.X.X..",
    "XXXXXXXXX",
    "..X.X.X..",
    ".X..X..X.",
    "X...X...X",
    "....X...."
  ],
  // Google Gemini — four-point AI spark.
  GEMINI: [
    "....X....",
    "....X....",
    "....X....",
    "...XXX...",
    "XXXXXXXXX",
    "...XXX...",
    "....X....",
    "....X....",
    "....X...."
  ],
  // Meta — infinity loop.
  LLAMA: [".XX...XX.", "X..X.X..X", "X..XXX..X", "X..X.X..X", ".XX...XX."],
  // xAI Grok — angular X.
  GROK: [
    "X.......X",
    ".X.....X.",
    "..X...X..",
    "...X.X...",
    "....X....",
    "...X.X...",
    "..X...X..",
    ".X.....X.",
    "X.......X"
  ],
  // DeepSeek — whale.
  DEEPSEEK: [
    "...XXXX..",
    "..XXXXXX.",
    ".XXXXXXXX",
    "XXXXXXX.X",
    ".XXXXXXXX",
    "..XX..XX.",
    "...X..X.."
  ],
  // Qwen — Q monogram.
  QWEN: [
    "..XXXXX..",
    ".X.....X.",
    "X.......X",
    "X.......X",
    "X...X...X",
    "X....X..X",
    ".X....X.X",
    "..XXXXX..",
    "......XXX"
  ],
  // Cohere — open ring / C.
  COHERE: [
    "..XXXXX..",
    ".X.....X.",
    "X........",
    "X........",
    "X........",
    "X........",
    ".X.....X.",
    "..XXXXX.."
  ],
  // Tavily — magnifier (search).
  TAVILY: [
    ".XXXX....",
    "X....X...",
    "X....X...",
    "X....X...",
    ".XXXX....",
    "....XX...",
    ".....XX..",
    "......XX.",
    ".......XX"
  ],
  // Brave — lion head + mane.
  BRAVE: [
    "X.X.X.X.X",
    ".XXXXXXX.",
    "XX.X.X.XX",
    "XXXXXXXXX",
    "X.XXXXX.X",
    ".XX...XX.",
    "..XXXXX..",
    "...X.X..."
  ],
  // Exa — node graph (embeddings).
  EXA: [
    "X.......X",
    ".X.....X.",
    "..X...X..",
    "...XXX...",
    "..XX.XX..",
    "...XXX...",
    "..X...X..",
    ".X.....X.",
    "X.......X"
  ],
  // Serper — S monogram.
  SERPER: [".XXXXX..", "X.......", "X.......", ".XXXXX..", "......X.", "......X.", ".XXXXX.."],
  // Perplexity — nested/rotational squares.
  PPLX: [
    "XXXXXXXXX",
    "X.......X",
    "X.XXXXX.X",
    "X.X...X.X",
    "X.X.X.X.X",
    "X.X...X.X",
    "X.XXXXX.X",
    "X.......X",
    "XXXXXXXXX"
  ],
  // Firecrawl — flame.
  FIRECRAWL: [
    "....X....",
    "...XX....",
    "...XXX...",
    "..XXXX...",
    "..XXXXX..",
    ".XXX.XX..",
    ".XXXXXX..",
    "..XXXX...",
    "...XX...."
  ],
  // Jina — J monogram.
  JINA: ["..XXXXX..", "....X....", "....X....", "X...X....", "X...X....", ".XXX....."],
  // Bing — b monogram.
  BING: ["X........", "X........", "X........", "XXXXX....", "X....X...", "X....X...", "XXXXX...."]
};

// Multi-colour marks get their own draw functions.
const MISTRAL_GRID = ["XXX......", "XXXYYY...", "XXXYYYOOO", "...YYYOOO", "......OOO"];
const MS_GRID = ["RRR.GGG", "RRR.GGG", "RRR.GGG", ".......", "BBB.YYY", "BBB.YYY", "BBB.YYY"];
const DEFAULT_GRID = [
  "....X....",
  "....X....",
  "...XXX...",
  "XXXXXXXXX",
  ".XXXXXXX.",
  "..XX.XX..",
  ".XX...XX.",
  ".X.....X.",
  "X.......X"
];

const BRAND_SPRITES = {};
for (const label of Object.keys(GLYPHS)) {
  const grid = GLYPHS[label];
  BRAND_SPRITES[label] = (ctx, cx, cy, col) => drawBitmap(ctx, grid, cx, cy, 3, { X: col });
}
// Mistral — stepped orange→amber stripes.
BRAND_SPRITES.MISTRAL = (ctx, cx, cy) =>
  drawBitmap(ctx, MISTRAL_GRID, cx, cy, 3, { X: "#fa5310", Y: "#ff8a4d", O: "#ffd23d" });
// Microsoft Copilot — the four-square mark.
BRAND_SPRITES.COPILOT = (ctx, cx, cy) =>
  drawBitmap(ctx, MS_GRID, cx, cy, 3, { R: "#f25022", G: "#7fba00", B: "#00a4ef", Y: "#ffb900" });

function drawDefaultSprite(ctx, cx, cy, col) {
  drawBitmap(ctx, DEFAULT_GRID, cx, cy, 3, { X: col });
}

function normalizeKey(key) {
  if (key === "ArrowLeft" || key === "a" || key === "A") return "left";
  if (key === "ArrowRight" || key === "d" || key === "D") return "right";
  if (key === " ") return "fire";
  return key;
}

function overlaps(a, b) {
  return a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y;
}

function clamp(v, lo, hi) {
  return Math.max(lo, Math.min(hi, v));
}

// Deterministic-enough randomness without Math.random gymnastics; a plain LCG
// seeded off performance.now so runs vary but nothing here needs crypto.
let seed = ((typeof performance !== "undefined" ? performance.now() : 1) % 2147483647) | 0;
if (seed <= 0) seed += 2147483646;
function rand() {
  seed = (seed * 16807) % 2147483647;
  return (seed - 1) / 2147483646;
}

function makeStarfield(n) {
  const stars = [];
  for (let i = 0; i < n; i += 1) {
    stars.push({
      x: rand() * VIEW_W,
      y: rand() * VIEW_H,
      size: rand() < 0.8 ? 1 : 2,
      a: 0.2 + rand() * 0.6,
      speed: 6 + rand() * 20
    });
  }
  return stars;
}

function readHiScore() {
  try {
    return parseInt(window.localStorage.getItem(HI_SCORE_KEY) || "0", 10) || 0;
  } catch {
    return 0;
  }
}

function writeHiScore(value) {
  try {
    window.localStorage.setItem(HI_SCORE_KEY, String(value));
  } catch {
    // Ignore storage failures (private mode etc.).
  }
}

// Tiny WebAudio blip generator. All calls are guarded so a missing/blocked
// AudioContext never breaks the game.
class Blipper {
  constructor() {
    this.enabled = true;
    this.ctx = null;
  }

  ensure() {
    if (this.ctx) return this.ctx;
    const Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return null;
    try {
      this.ctx = new Ctx();
    } catch {
      this.ctx = null;
    }
    return this.ctx;
  }

  toggle() {
    this.enabled = !this.enabled;
  }

  blip(freq, dur, type) {
    if (!this.enabled) return;
    const ctx = this.ensure();
    if (!ctx) return;
    try {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = type || "square";
      osc.frequency.value = freq;
      gain.gain.value = 0.04;
      gain.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + dur);
      osc.connect(gain).connect(ctx.destination);
      osc.start();
      osc.stop(ctx.currentTime + dur);
    } catch {
      // Ignore audio failures.
    }
  }

  win() {
    [523, 659, 784, 1047].forEach((f, i) => {
      window.setTimeout(() => this.blip(f, 0.12), i * 110);
    });
  }
}
