#!/usr/bin/env node

const fs = require("fs");
const http = require("http");
const path = require("path");
const { spawn } = require("child_process");

function parseArgs(argv) {
  const args = {
    cwd: process.cwd(),
    url: process.env.SLAG_OUTCOME_URL || "http://127.0.0.1:5173",
    waitMs: Number.parseInt(process.env.SLAG_OUTCOME_WAIT_MS || "2500", 10),
    timeoutMs: Number.parseInt(process.env.SLAG_OUTCOME_TIMEOUT_MS || "45000", 10),
    startCmd: process.env.SLAG_OUTCOME_WEB_START || "",
  };
  for (let i = 2; i < argv.length; i += 1) {
    const key = argv[i];
    const value = argv[i + 1];
    if (key === "--cwd" && value) {
      args.cwd = path.resolve(value);
      i += 1;
    } else if (key === "--url" && value) {
      args.url = value;
      i += 1;
    } else if (key === "--wait-ms" && value) {
      args.waitMs = Number.parseInt(value, 10);
      i += 1;
    } else if (key === "--timeout-ms" && value) {
      args.timeoutMs = Number.parseInt(value, 10);
      i += 1;
    } else if (key === "--start" && value) {
      args.startCmd = value;
      i += 1;
    }
  }
  return args;
}

function findDevCommand(cwd) {
  const packageJsonPath = path.join(cwd, "package.json");
  if (!fs.existsSync(packageJsonPath)) {
    return null;
  }
  try {
    const pkg = JSON.parse(fs.readFileSync(packageJsonPath, "utf8"));
    if (pkg && pkg.scripts && pkg.scripts.dev) {
      return "npm run dev -- --host 127.0.0.1 --port 5173";
    }
  } catch (_) {
    return null;
  }
  return null;
}

function waitForUrl(targetUrl, timeoutMs) {
  return new Promise((resolve, reject) => {
    const deadline = Date.now() + timeoutMs;
    const tick = () => {
      const req = http.get(targetUrl, (res) => {
        res.resume();
        if (res.statusCode && res.statusCode < 500) {
          resolve();
        } else if (Date.now() > deadline) {
          reject(new Error(`timeout waiting for ${targetUrl}`));
        } else {
          setTimeout(tick, 300);
        }
      });
      req.on("error", () => {
        if (Date.now() > deadline) {
          reject(new Error(`timeout waiting for ${targetUrl}`));
        } else {
          setTimeout(tick, 300);
        }
      });
      req.setTimeout(1500, () => {
        req.destroy();
      });
    };
    tick();
  });
}

function startStaticServer(cwd, port) {
  const mime = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".svg": "image/svg+xml",
    ".woff": "font/woff",
    ".woff2": "font/woff2",
  };

  const server = http.createServer((req, res) => {
    const reqUrl = (req.url || "/").split("?")[0];
    const rel = reqUrl === "/" ? "/index.html" : decodeURIComponent(reqUrl);
    const filePath = path.join(cwd, rel);
    if (!filePath.startsWith(cwd)) {
      res.statusCode = 403;
      res.end("forbidden");
      return;
    }
    fs.readFile(filePath, (err, data) => {
      if (err) {
        res.statusCode = 404;
        res.end("not found");
        return;
      }
      const ext = path.extname(filePath).toLowerCase();
      res.setHeader("Content-Type", mime[ext] || "application/octet-stream");
      res.end(data);
    });
  });

  return new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(port, "127.0.0.1", () => resolve(server));
  });
}

function stopChild(child) {
  if (!child || child.killed) return;
  child.kill("SIGTERM");
  setTimeout(() => {
    if (!child.killed) {
      child.kill("SIGKILL");
    }
  }, 2000).unref();
}

async function run() {
  const args = parseArgs(process.argv);
  const screenshotPath = process.env.SLAG_OUTCOME_SCREENSHOT || "logs/outcome-smoke.png";
  const metricRegex = new RegExp(
    process.env.SLAG_OUTCOME_METRIC_REGEX ||
      "(snakes?|entities?|agents?|bots?|players?)\\s*[:=]\\s*(\\d+)",
    "i",
  );
  const urlObj = new URL(args.url);
  const port = Number.parseInt(urlObj.port || "5173", 10);

  let devProc = null;
  let staticServer = null;

  try {
    if (!args.startCmd) {
      args.startCmd = findDevCommand(args.cwd) || "";
    }

    if (args.startCmd) {
      devProc = spawn("bash", ["-lc", args.startCmd], {
        cwd: args.cwd,
        stdio: ["ignore", "pipe", "pipe"],
      });
    } else if (fs.existsSync(path.join(args.cwd, "index.html"))) {
      staticServer = await startStaticServer(args.cwd, port);
    }

    await waitForUrl(args.url, args.timeoutMs);

    let chromium;
    try {
      ({ chromium } = require("playwright"));
    } catch (err) {
      throw new Error(`playwright is required for outcome smoke test: ${err.message}`);
    }

    const browser = await chromium.launch({ headless: true });
    const page = await browser.newPage();
    const consoleErrors = [];
    page.on("console", (m) => {
      if (m.type() === "error") {
        consoleErrors.push(m.text());
      }
    });
    page.on("pageerror", (e) => {
      consoleErrors.push(`pageerror:${e.message}`);
    });

    await page.goto(args.url, { waitUntil: "domcontentloaded" });
    await page.waitForTimeout(args.waitMs);

    const statsText = await page
      .locator("#stats")
      .first()
      .innerText()
      .catch(() => "");
    const bodyText = (await page.textContent("body")) || "";
    const combined = `${statsText}\n${bodyText}`;
    const match = combined.match(metricRegex);
    const metricLabel = match ? match[1] : "unknown";
    const metricValue = match ? Number.parseInt(match[2], 10) : 0;

    fs.mkdirSync(path.dirname(screenshotPath), { recursive: true });
    await page.screenshot({ path: screenshotPath, fullPage: true });
    await browser.close();

    if (consoleErrors.length > 0) {
      throw new Error(`console errors detected: ${consoleErrors.length}`);
    }
    if (!match) {
      throw new Error("no runtime metric found in page text");
    }
    if (!(metricValue > 0)) {
      throw new Error(`runtime metric ${metricLabel}=${metricValue} is not > 0`);
    }
    if (!fs.existsSync(screenshotPath) || fs.statSync(screenshotPath).size <= 0) {
      throw new Error(`screenshot missing or empty at ${screenshotPath}`);
    }

    console.log(
      JSON.stringify(
        {
          ok: true,
          url: args.url,
          screenshot: screenshotPath,
          metricLabel,
          metricValue,
          consoleErrors: 0,
        },
        null,
        2,
      ),
    );
  } finally {
    stopChild(devProc);
    if (staticServer) {
      await new Promise((resolve) => staticServer.close(resolve));
    }
  }
}

run().catch((err) => {
  console.error(
    JSON.stringify(
      {
        ok: false,
        error: err.message,
        screenshot: process.env.SLAG_OUTCOME_SCREENSHOT || "logs/outcome-smoke.png",
      },
      null,
      2,
    ),
  );
  process.exit(1);
});
