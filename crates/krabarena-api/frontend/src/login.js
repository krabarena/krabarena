import {
  clearAuthSession,
  getAuthSession,
  setAuthSession,
  setLoginMethod,
  setLoginReturn,
  takeLoginMethod,
  takeLoginReturn
} from "./session.js";
import {
  getCurrentVoter,
  sendDeviceEmailLink as sendDeviceEmailLinkRequest,
  sendVoterEmailCode,
  verifyVoterEmailCode
} from "./api.js";
import { track } from "./analytics.js";
import { validateEmail, validateEmailFormat } from "./email-validation.js";

export function initLogin() {
  initLoginPage();
  initLoginCallback();
  initAuthEmailVerified();
  initDeviceLogin();
  initAboutLogin();
}

// reCAPTCHA Enterprise (score-based). `enterprise.js?render=SITE_KEY` (loaded
// by partials/recaptcha.html only when RECAPTCHA_SITE_KEY is configured)
// exposes `grecaptcha.enterprise`; we mint a fresh token per send via
// `execute(siteKey, {action})`. No widget, no explicit render, no reset.
// Upper bound on how long we wait for a token before giving up (and letting
// the backend reject), so a stalled call can't wedge sign-in.
const CAPTCHA_TIMEOUT_MS = 30000;
// How often to re-check for the async-`defer` enterprise.js script while it
// finishes loading (a fast submit can beat it to the punch).
const CAPTCHA_POLL_MS = 100;

// The site key is handed over by the server-rendered partial; absent means
// captcha is disabled (no script loaded).
function recaptchaSiteKey() {
  return document.getElementById("recaptcha-config")?.dataset.sitekey || null;
}

// Resolves with a reCAPTCHA Enterprise token, or null when captcha is disabled
// (no site key) or unavailable (the script never loads, or execute stalls past
// the timeout). Because enterprise.js loads `async defer`, a submit right after
// typing can arrive before `grecaptcha.enterprise` exists — so we poll for it
// up to the timeout instead of giving up immediately (which would send no token
// and get a spurious CAPTCHA_FAILED). Callers treat null as "no token".
//
// Deps are injectable for unit testing (see login.test.js); production calls it
// with no args and reads the real `window`/`document`/`Date`.
export async function getCaptchaToken({
  siteKey = recaptchaSiteKey(),
  getApi = () => window.grecaptcha,
  timeoutMs = CAPTCHA_TIMEOUT_MS,
  pollMs = CAPTCHA_POLL_MS,
  now = () => Date.now(),
  sleep = (ms) => new Promise((r) => window.setTimeout(r, ms))
} = {}) {
  if (!siteKey) return null; // captcha disabled — no widget on the page

  // Wait out the async-defer script load until grecaptcha.enterprise is usable.
  const deadline = now() + timeoutMs;
  let enterprise = getApi()?.enterprise;
  while (!enterprise?.execute) {
    if (now() >= deadline) return null; // never loaded — let the backend decide
    await sleep(pollMs);
    enterprise = getApi()?.enterprise;
  }

  // Mint a fresh token; guard execute() so a stalled challenge can't wedge send.
  return new Promise((resolve) => {
    let done = false;
    const finish = (token) => {
      if (done) return;
      done = true;
      window.clearTimeout(timer);
      resolve(token);
    };
    const timer = window.setTimeout(() => finish(null), Math.max(pollMs, deadline - now()));
    try {
      enterprise.ready(() => {
        enterprise
          .execute(siteKey, { action: "voter_email" })
          .then((token) => finish(token || null))
          .catch(() => finish(null));
      });
    } catch {
      finish(null);
    }
  });
}

function initLoginPage() {
  const root = document.querySelector("[data-login-page]");
  if (!root) return;
  const returnTo = root.getAttribute("data-return-to") || "/";
  if (getAuthSession().voterToken) {
    window.location.href = returnTo;
    return;
  }

  root.querySelectorAll("[data-login-method]").forEach((link) => {
    link.addEventListener("click", () => {
      const method = link.getAttribute("data-login-method") || "oauth";
      setLoginReturn(returnTo);
      setLoginMethod(method);
      track("login_start", { method });
    });
  });

  const emailInput = document.getElementById("email-input");
  const codeInput = document.getElementById("code-input");
  const sendStep = document.getElementById("email-step-send");
  const verifyStep = document.getElementById("email-step-verify");
  const sentTo = document.getElementById("email-sent-to");
  const sendButton = document.querySelector("[data-email-send]");
  const verifyButton = document.querySelector("[data-email-verify]");
  const resetButton = document.querySelector("[data-email-reset]");

  const syncVerifyButton = () => {
    if (verifyButton) verifyButton.disabled = !hasValue(codeInput);
  };
  syncVerifyButton();

  // Keep the send button disabled until the field holds a well-formed
  // email. Format-only here (empty or malformed) — a valid-but-disposable
  // address still clicks through so the send path can surface its specific
  // "disposable address" message rather than a silently dead button.
  const syncSendButton = () => {
    if (sendButton) {
      sendButton.disabled = Boolean(validateEmailFormat((emailInput?.value || "").trim()));
    }
  };
  syncSendButton();

  // Guards against a second send firing while one is in flight. The send button
  // is type="submit" inside the form, so a click raises BOTH this and the form's
  // submit event — without this we'd POST twice (two codes emailed).
  let sending = false;
  const doSend = async () => {
    if (sending) return;
    sending = true;
    if (sendButton) sendButton.disabled = true;
    try {
      await sendEmailCode({
        emailInput,
        acquireCaptchaToken: getCaptchaToken,
        setMessage: setLoginMessage,
        onSent(email) {
          if (sentTo) sentTo.textContent = email;
          if (sendStep) sendStep.hidden = true;
          if (verifyStep) verifyStep.hidden = false;
          clearCodeError(codeInput);
          syncVerifyButton();
          codeInput?.focus();
        }
      });
    } finally {
      sending = false;
      syncSendButton();
    }
  };

  const doVerify = () =>
    verifyEmailCode({
      emailInput,
      codeInput,
      setMessage: setLoginMessage,
      onSuccess() {
        window.location.href = returnTo;
      }
    });

  // Only listen on the form's submit — the send button is type="submit", so its
  // click already raises submit. Binding click too would run doSend twice.
  sendStep?.addEventListener("submit", (e) => {
    e.preventDefault();
    doSend();
  });

  verifyButton?.addEventListener("click", doVerify);
  verifyStep?.addEventListener("submit", (e) => {
    e.preventDefault();
    doVerify();
  });

  resetButton?.addEventListener("click", () => {
    if (codeInput) codeInput.value = "";
    if (verifyStep) verifyStep.hidden = true;
    if (sendStep) sendStep.hidden = false;
    setLoginMessage("");
    clearEmailError(emailInput);
    clearCodeError(codeInput);
    syncVerifyButton();
    syncSendButton();
    emailInput?.focus();
  });

  emailInput?.addEventListener("focus", () => clearEmailError(emailInput));
  emailInput?.addEventListener("input", () => {
    clearEmailError(emailInput);
    syncSendButton();
  });
  emailInput?.addEventListener("blur", () => {
    const val = (emailInput.value || "").trim();
    if (!val) return;
    const formatErr = validateEmailFormat(val);
    if (formatErr) setEmailError(emailInput, formatErr, setLoginMessage);
  });
  codeInput?.addEventListener("input", () => {
    clearCodeError(codeInput);
    syncVerifyButton();
  });
}

function initAboutLogin() {
  const root = document.querySelector("[data-about-login]");
  if (!root) return;
  const auth = getAuthSession();
  const status = document.getElementById("voter-status");
  const loginForm =
    document.getElementById("voter-login-form") || document.getElementById("logged-out");
  const logoutForm =
    document.getElementById("voter-logout-form") || document.getElementById("logged-in");
  const voterName = document.getElementById("voter-name");
  if (auth.voterToken) {
    if (status) status.textContent = `Signed in as ${auth.voterName || "voter"}`;
    if (voterName) voterName.textContent = auth.voterName || "voter";
    if (loginForm) loginForm.hidden = true;
    if (logoutForm) logoutForm.hidden = false;
  }

  root.querySelectorAll("[data-login-method]").forEach((link) => {
    link.addEventListener("click", () => {
      const method = link.getAttribute("data-login-method") || "oauth";
      setLoginMethod(method);
      track("login_start", { method });
    });
  });

  document.querySelector("[data-about-email-send]")?.addEventListener("click", () => {
    sendEmailCode({
      emailInput: document.getElementById("voter-email") || document.getElementById("login-email"),
      acquireCaptchaToken: getCaptchaToken,
      setMessage: setAboutMessage,
      onSent() {
        const codeRow =
          document.getElementById("voter-code-row") || document.getElementById("email-step2");
        if (codeRow) codeRow.hidden = false;
      }
    });
  });

  document.querySelector("[data-about-email-verify]")?.addEventListener("click", () => {
    verifyEmailCode({
      emailInput: document.getElementById("voter-email") || document.getElementById("login-email"),
      codeInput: document.getElementById("voter-code") || document.getElementById("login-code"),
      setMessage: setAboutMessage,
      onSuccess() {
        window.location.reload();
      }
    });
  });

  document.querySelector("[data-about-logout]")?.addEventListener("click", () => {
    clearAuthSession();
    window.location.reload();
  });
}

// The OAuth callback delivers the voter session in the URL *fragment*
// (`/login/callback#token=…`), never the query string: fragments are not
// sent to the server, so the token stays out of access logs, proxies and
// the Referer header. The HttpOnly `ka_session` cookie is the primary
// credential; this copy only feeds the legacy localStorage session.
export function readOAuthCallbackParams(location) {
  const hash = (location?.hash || "").replace(/^#/, "");
  const params = new URLSearchParams(hash);
  return { token: params.get("token"), name: params.get("name") };
}

async function initLoginCallback() {
  const root = document.querySelector("[data-oauth-callback]");
  if (!root) return;
  const msg = document.getElementById("msg");
  const { token, name } = readOAuthCallbackParams(window.location);
  // Scrub the session token from the address bar / history right away.
  try {
    window.history.replaceState(null, "", window.location.pathname);
  } catch {
    // Non-fatal: older browsers / sandboxed frames.
  }
  if (!token) {
    if (msg) msg.textContent = "Login failed - no token received.";
    return;
  }

  setAuthSession({ voterToken: token, voterName: name });
  track("login_success", { method: takeLoginMethod("oauth") });
  try {
    const response = await getCurrentVoter();
    const body = response.data || {};
    if (body.data) {
      setAuthSession({
        voterName: body.data.display_name,
        adminToken: body.data.admin_token
      });
    }
  } catch {
    // Login already succeeded; enrich session only when this compatibility lookup works.
  }
  redirectAfterLogin(msg);
}

function initAuthEmailVerified() {
  const root = document.querySelector("[data-auth-email-verified]");
  if (!root) return;
  setAuthSession({
    voterToken: root.getAttribute("data-voter-token"),
    voterName: root.getAttribute("data-voter-name"),
    adminToken: root.getAttribute("data-admin-token")
  });
  track("login_success", { method: "email" });
}

function initDeviceLogin() {
  const root = document.querySelector("[data-device-login]");
  if (!root) return;

  root.querySelectorAll("[data-login-method]").forEach((link) => {
    link.addEventListener("click", () => {
      const method = link.getAttribute("data-login-method") || "oauth";
      setLoginMethod(method);
      track("login_start", { method, flow: "device" });
    });
  });

  root.querySelector("[data-device-email-send]")?.addEventListener("click", () => {
    sendDeviceEmailLink({
      emailInput: document.getElementById("device-email-input"),
      setMessage: setDeviceMessage
    });
  });
}

function redirectAfterLogin(msg) {
  if (msg) msg.textContent = "Logged in! Redirecting...";
  const returnUrl = takeLoginReturn("/");
  window.setTimeout(() => {
    window.location.href = returnUrl;
  }, 500);
}

async function sendEmailCode({ emailInput, acquireCaptchaToken, setMessage, onSent }) {
  const email = (emailInput?.value || "").trim();
  const validationError = validateEmail(email);
  if (validationError) {
    setEmailError(emailInput, validationError, setMessage);
    return;
  }
  track("login_start", { method: "email" });
  // Show progress BEFORE minting the captcha token. Acquiring it can stall for
  // up to CAPTCHA_TIMEOUT_MS when enterprise.js is blocked (adblocker) or slow,
  // and without this the button would look dead the whole time. Also runs after
  // validation, so a malformed/disposable address fails fast instead of waiting.
  setMessage("Sending...");
  try {
    const captchaToken = await acquireCaptchaToken();
    const response = await sendVoterEmailCode({ email, captchaToken });
    const body = response.data || {};
    if (!response.ok) {
      setMessage(body?.error?.message || "Failed to send code.", true);
      return;
    }
    clearEmailError(emailInput);
    setMessage("");
    onSent(email);
  } catch {
    setMessage("Network error - try again.", true);
  }
  // No token reset needed: score-based execute() mints a fresh token per send.
}

async function sendDeviceEmailLink({ emailInput, setMessage }) {
  const email = (emailInput?.value || "").trim();
  const validationError = validateEmail(email);
  if (validationError) {
    setEmailError(emailInput, validationError, setMessage);
    return;
  }
  track("login_start", { method: "email", flow: "device" });
  setMessage("Sending...");
  try {
    const response = await sendDeviceEmailLinkRequest({ email });
    const body = response.data || {};
    if (!response.ok) {
      setMessage(body?.error?.message || "Failed to send link.", true);
      return;
    }
    setMessage("Check your inbox and click the link. The CLI will detect it automatically.");
  } catch {
    setMessage("Network error - try again.", true);
  }
}

async function verifyEmailCode({ emailInput, codeInput, setMessage, onSuccess }) {
  const email = (emailInput?.value || "").trim();
  const code = (codeInput?.value || "").trim();
  if (code.length !== 8) {
    setCodeError(codeInput, "Enter the 8-digit code from your email.", setMessage);
    return;
  }
  clearCodeError(codeInput);
  setMessage("Verifying...");
  try {
    const response = await verifyVoterEmailCode({ email, code });
    const body = response.data || {};
    if (!response.ok) {
      setMessage(body?.error?.message || "Invalid or expired code.", true);
      return;
    }
    const data = body.data;
    if (!data?.voter_token) {
      setMessage("Sign-in failed - no token received.", true);
      return;
    }
    setAuthSession({
      voterToken: data.voter_token,
      voterName: data.display_name,
      adminToken: data.admin_token
    });
    track("login_success", { method: "email" });
    setMessage("Signed in! Redirecting...");
    onSuccess();
  } catch {
    setMessage("Network error - try again.", true);
  }
}

function setLoginMessage(text, isError) {
  const el = document.getElementById("email-msg");
  if (!el) return;
  el.textContent = text;
  setStatusClass(el, isError);
}

function setDeviceMessage(text, isError) {
  const el = document.getElementById("device-email-msg");
  if (!el) return;
  el.textContent = text;
  setStatusClass(el, isError);
}

function setAboutMessage(text, isError) {
  const targets = [
    document.getElementById("voter-msg"),
    document.getElementById("email-status"),
    document.getElementById("verify-status")
  ].filter(Boolean);
  targets.forEach((el) => {
    el.textContent = text;
    setStatusClass(el, isError, true);
  });
}

function setStatusClass(el, isError, successWhenNotError = false) {
  el.classList.toggle("is-error", Boolean(isError));
  el.classList.toggle("is-success", !isError && successWhenNotError && Boolean(el.textContent));
}

function setEmailError(input, message, setMessage) {
  setInlineFieldError({
    input,
    inlineId: "email-inline-error",
    message,
    setMessage
  });
}

function clearEmailError(input) {
  clearInlineFieldError(input, "email-inline-error");
}

function setCodeError(input, message, setMessage) {
  setInlineFieldError({
    input,
    inlineId: "code-inline-error",
    message,
    setMessage
  });
}

function clearCodeError(input) {
  clearInlineFieldError(input, "code-inline-error");
}

function setInlineFieldError({ input, inlineId, message, setMessage }) {
  if (input) {
    input.classList.add("is-invalid");
    input.setAttribute("aria-invalid", "true");
  }
  const inlineEl = document.getElementById(inlineId);
  if (inlineEl) {
    inlineEl.textContent = message;
    inlineEl.hidden = false;
    setMessage("", false);
  } else {
    setMessage(message, true);
  }
}

function clearInlineFieldError(input, inlineId) {
  if (input) {
    input.classList.remove("is-invalid");
    input.setAttribute("aria-invalid", "false");
  }
  const inlineEl = document.getElementById(inlineId);
  if (inlineEl) {
    inlineEl.textContent = "";
    inlineEl.hidden = true;
  }
}

function hasValue(input) {
  return Boolean((input?.value || "").trim());
}
