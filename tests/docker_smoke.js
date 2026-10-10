// Run inside a client container on the disposable Compose network.
const base = process.env.SPECULUM_TEST_BASE || "http://speculum:8080";
function assert(condition, message) {
  if (!condition) throw new Error(message);
}
async function request(path, method = "GET", body, session, expected = 200) {
  const response = await fetch(base + path, {
    method,
    headers: {
      "Content-Type": "application/json",
      ...(session
        ? { Cookie: session.cookie, "X-CSRF-Token": session.body.csrf_token }
        : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  assert(
    response.status === expected,
    `Unexpected HTTP status ${response.status} for ${path}`,
  );
  const content = await response.json().catch(() => undefined);
  return {
    body: content,
    cookie: response.headers.get("set-cookie")?.split(";")[0],
  };
}
async function login(role, username, password) {
  return request(`/api/${role}/login`, "POST", { username, password });
}
if (process.env.SPECULUM_TEST_PHASE === "restart") {
  const admin = await login("admin", "admin", "docker-admin-password");
  assert(
    admin.body.must_change_password === false,
    "Admin password state did not persist",
  );
  const user = await login("user", "docker-user", "docker-user-password");
  assert(
    user.body.must_change_password === false,
    "User password state did not persist",
  );
  const tokens = await request("/api/user/tokens", "GET", undefined, user);
  assert(tokens.body.length === 1, "User tokens did not persist");
  console.log("PASS: Compose restart preserves chosen passwords and tokens");
} else {
  let admin = await login("admin", "admin", process.env.SPECULUM_TEST_PASSWORD);
  assert(
    admin.body.must_change_password === true,
    "Initial admin must change password",
  );
  await request("/api/admin/settings", "GET", undefined, admin, 403);
  admin = await request(
    "/api/admin/credentials",
    "PUT",
    {
      current_password: process.env.SPECULUM_TEST_PASSWORD,
      password: "docker-admin-password",
    },
    admin,
  );
  assert(admin.body.must_change_password === false, "Admin onboarding failed");
  const created = await request(
    "/api/admin/users",
    "POST",
    { username: "docker-user", enabled: true },
    admin,
    201,
  );
  let user = await login(
    "user",
    "docker-user",
    created.body.temporary_password,
  );
  assert(
    user.body.must_change_password === true,
    "Initial user must change password",
  );
  await request("/api/user/tokens", "POST", { name: "blocked" }, user, 403);
  user = await request(
    "/api/user/password",
    "PUT",
    {
      current_password: created.body.temporary_password,
      password: "docker-user-password",
    },
    user,
  );
  const token = await request(
    "/api/user/tokens",
    "POST",
    { name: "Docker smoke test" },
    user,
  );
  assert(token.body.token.startsWith("spm_"), "Token issuance failed");
  const listing = await request("/api/user/tokens", "GET", undefined, user);
  assert(
    listing.body.length === 1 &&
      !listing.body[0].token &&
      !listing.body[0].hash,
    "Token list exposed secrets",
  );
  await request("/api/admin/settings", "GET", undefined, user, 401);
  await request("/api/user/session", "GET", undefined, admin, 401);
  const summary = await request("/api/user/summary", "GET", undefined, user);
  assert(
    summary.body.total.requests === 0 &&
      summary.body.total.stored_bytes === undefined,
    "Personal summary is incorrect",
  );
  const config = await request("/api/user/config", "GET", undefined, user);
  assert(
    Object.keys(config.body).length === 2 &&
      Object.keys(config.body.registries.pypi).length === 2,
    "Configuration exposed internal settings",
  );
  for (const page of ["/admin", "/account"]) {
    const response = await fetch(base + page);
    assert(
      response.ok && (await response.text()).includes("Speculum"),
      "Dashboard asset unavailable",
    );
  }
  console.log(
    "PASS: Compose bootstrap, forced password changes, user tokens, role isolation, and embedded dashboards",
  );
}
