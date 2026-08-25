// Regenerates the README graphics from their HTML sources.
//
//   cd docs && npm install && node build-graphics.mjs
//
// hero.html -> hero.png  (1280x640, also the GitHub social preview)
// demo.html -> demo.gif  (one screenshot per frame, quantized to a shared palette)
//
// Frames come from the BEATS table in demo.html; BEAT_FRAMES below must match its
// `n` values, since a headless screenshot can't hand a count back to this script.

import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { PNG } from 'pngjs';
import { GIFEncoder, quantize, applyPalette } from 'gifenc/dist/gifenc.esm.js'; // package `main` is CJS; named imports need the ESM build

const DOCS = dirname(fileURLToPath(import.meta.url));

const HERO = { w: 1280, h: 640 };
const DEMO = { w: 1000, h: 360 };
/** Frames per beat in demo.html's BEATS, and how long each beat's frames hold. */
const BEAT_FRAMES = [5, 3, 3, 6, 3, 3];
const BEAT_DELAY_MS = [400, 400, 400, 500, 400, 500];

const BROWSERS = [
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Microsoft/Edge/Application/msedge.exe',
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
];

const browser = BROWSERS.find(existsSync);
if (!browser) throw new Error('No Edge or Chrome found; add its path to BROWSERS.');

const profileRoot = mkdtempSync(join(tmpdir(), 'claude-tray-render-'));
const profile = join(profileRoot, 'profile');
const frameDir = mkdtempSync(join(tmpdir(), 'claude-tray-frames-'));

const sleep = ms => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);

function edge(args) {
  execFileSync(browser, [
    '--headless=new', '--disable-gpu', '--hide-scrollbars',
    '--no-first-run', '--no-default-browser-check',
    '--force-color-profile=srgb',
    `--user-data-dir=${profile}`,
    ...args,
  ], { stdio: 'ignore' });
}

/**
 * Screenshot one page at an exact size.
 *
 * Both failure modes here are startup races, not page problems: a cold profile can
 * exit 0 having written nothing, and a profile still locked by the previous launch
 * exits 21. Retrying the same command clears both.
 */
function shoot(url, out, { w, h }) {
  for (let attempt = 1; attempt <= 4; attempt++) {
    try {
      edge([`--window-size=${w},${h}`, `--screenshot=${out}`, url]);
      if (existsSync(out)) return;
    } catch { /* exit 21 and friends: fall through to the retry */ }
    sleep(400 * attempt);
  }
  throw new Error(`no screenshot produced for ${url} after 4 attempts`);
}

function pageUrl(file, query = '') {
  return pathToFileURL(resolve(DOCS, file)).href + query;
}

edge(['--window-size=200,200', '--dump-dom', 'about:blank']);  // warm the profile

// ---- hero.png ---------------------------------------------------------------
const heroOut = join(DOCS, 'hero.png');
shoot(pageUrl('hero.html'), heroOut, HERO);
console.log(`hero.png  ${(readFileSync(heroOut).length / 1024).toFixed(0)} KB`);

// ---- demo.gif ---------------------------------------------------------------
const delays = BEAT_FRAMES.flatMap((n, b) => Array(n).fill(BEAT_DELAY_MS[b]));
const total = delays.length;

const frames = [];
for (let i = 0; i < total; i++) {
  const out = join(frameDir, `f${String(i).padStart(3, '0')}.png`);
  shoot(pageUrl('demo.html', `?frame=${i}`), out, DEMO);
  const png = PNG.sync.read(readFileSync(out));
  if (png.width !== DEMO.w || png.height !== DEMO.h) {
    throw new Error(`frame ${i} is ${png.width}x${png.height}, expected ${DEMO.w}x${DEMO.h}`);
  }
  frames.push(new Uint8Array(png.data));
  process.stdout.write(`\rframe ${i + 1}/${total}`);
}
process.stdout.write('\n');

// One palette for the whole animation, sampled across every frame so colors don't
// shift between them. Stride keeps the quantizer's input to a sane size.
const STRIDE = 4;
const perFrame = Math.floor(frames[0].length / 4 / STRIDE);
const sample = new Uint8Array(perFrame * frames.length * 4);
let at = 0;
for (const f of frames) {
  for (let p = 0; p < perFrame; p++) {
    sample.set(f.subarray(p * STRIDE * 4, p * STRIDE * 4 + 4), at);
    at += 4;
  }
}
const palette = quantize(sample, 256, { format: 'rgb565' });

const gif = GIFEncoder();
frames.forEach((f, i) => {
  gif.writeFrame(applyPalette(f, palette, 'rgb565'), DEMO.w, DEMO.h, {
    palette, delay: delays[i], repeat: 0,
  });
});
gif.finish();

const gifOut = join(DOCS, 'demo.gif');
writeFileSync(gifOut, gif.bytes());
console.log(`demo.gif  ${(readFileSync(gifOut).length / 1024).toFixed(0)} KB, ${total} frames, ` +
            `${(delays.reduce((a, b) => a + b, 0) / 1000).toFixed(1)}s loop`);

// Edge's background processes can still hold the profile; these are temp dirs, so a
// failed cleanup is not worth failing the build over.
for (const dir of [profileRoot, frameDir]) {
  try { rmSync(dir, { recursive: true, force: true }); } catch { /* the OS will get it */ }
}
