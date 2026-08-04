# Windows install fails: `schtasks /Create ... /XML` — two bugs

Status: **diagnosed, not fixed.** Both root causes reproduced and both fixes
verified empirically on a real Windows 11 box (HAREBELL, unelevated
Administrator). Everything below is confirmed by test, not inferred.

## Symptom

`irm https://ccscreen.dibbla.work/install.ps1?name=harebell-win | iex` prints:

```
ERROR: The task XML is malformed.
(1,40)::ERROR: unable to switch the encoding
install failed: `schtasks /Create /TN cc-screen-rust /XML C:\Users\erik\.config\cc-screen-rust\task.xml /F` failed (exit code: 1)

OK — 'harebell-win' is connected and will reconnect automatically.
```

Enrollment genuinely succeeds. Service registration does not. No task exists
afterwards (`schtasks /Query /TN cc-screen-rust` → "cannot find the file
specified"), so the agent never starts at logon.

There are **two independent bugs**. The second is hidden behind the first — fix
only bug 1 and you get a fresh `Access is denied` failure.

---

## Bug 1 — task.xml is written UTF-8; `schtasks` requires UTF-16LE

`install_windows_service` (`src/service.rs:606`) writes the file with
`std::fs::write`, which emits raw UTF-8 with no BOM. `schtasks /XML` reads task
files as **UTF-16LE**. The parser fails at line 1 column 40 — exactly where
`encoding="UTF-8"` sits in the declaration.

**Important, and the non-obvious part:** writing UTF-16LE bytes *alone does not
fix it*. Verified — it produces the identical error, because the UTF-16 BOM then
contradicts the `encoding="UTF-8"` declaration and the parser fails at the same
offset. **Both halves are required together:**

1. the declaration must say `encoding="UTF-16"`, and
2. the bytes must be UTF-16LE **with** a BOM.

### Fix

In `windows_task_xml` (`src/service.rs:376`), change the declaration:

```rust
r#"<?xml version="1.0" encoding="UTF-16"?>
```

Add a helper next to it:

```rust
/// `schtasks /XML` only accepts UTF-16LE-with-BOM. `std::fs::write` would emit
/// UTF-8 and the parser dies at the encoding declaration, so encode explicitly.
fn write_utf16le_bom(path: &Path, s: &str) -> std::io::Result<()> {
    let mut bytes = vec![0xFF, 0xFE]; // BOM
    for unit in s.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    std::fs::write(path, bytes)
}
```

And in `install_windows_service`, swap the write:

```rust
write_utf16le_bom(&xml_path, &xml)
    .map_err(|e| format!("writing {}: {e}", xml_path.display()))?;
```

---

## Bug 2 — `<LogonTrigger>` without `<UserId>` requires elevation

Once bug 1 is fixed the XML parses, and `schtasks` then fails with
`ERROR: Access is denied`.

A `<LogonTrigger>` with **no** `<UserId>` means *"when **any** user logs on"* — a
machine-wide trigger, which requires an elevated token. The user here is in the
Administrators group but runs under a UAC-filtered token (`Medium Mandatory
Level`; Administrators shown as *"used for deny only"*), so it is denied. This
contradicts the comment above `install_windows_service`, which claims the task
"needs no elevation".

Isolated by test: an `ONLOGON` task was denied unelevated while an otherwise
identical `ONCE` task created successfully.

### Fix

Scope the trigger to the current user. **The `<UserId>` must go inside the
`<LogonTrigger>`, not inside the `<Principal>`** — putting it in the Principal
was tested and is *still denied*. This is the single most likely thing to get
wrong.

```xml
<Triggers>
  <LogonTrigger>
    <UserId>{user}</UserId>
    <Enabled>true</Enabled>
  </LogonTrigger>
</Triggers>
```

Derive the value at runtime — do not hardcode. Both `DOMAIN\user` and a bare
username were verified to work; prefer the qualified form, fall back to bare:

```rust
fn current_user() -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    match std::env::var("USERDOMAIN") {
        Ok(d) if !d.is_empty() && !user.is_empty() => format!("{d}\\{user}"),
        _ => user,
    }
}
```

Thread it into the `format!` in `windows_task_xml` as
`user = xml_escape(&current_user())`.

---

## Bug 3 (cosmetic but misleading) — failure reported as success

`scripts/install-machine.ps1:49` unconditionally prints

```
OK — '$Machine' is connected and will reconnect automatically.
```

even though `& $Bin install ...` on line 46 just failed. The "will reconnect
automatically" half is false — no task was registered. Gate that line on
`$LASTEXITCODE -eq 0` and print a real failure message otherwise, so the install
doesn't claim success it didn't achieve. `scripts/install-machine.sh:55` has the
same unconditional-echo shape and is worth the same treatment.

---

## Tests to update

`windows_task_xml_has_essentials` (`src/service.rs:861`) still passes after these
changes (it only asserts `x.contains("<LogonTrigger>")`, which remains true), so
it will **not** catch a regression. Add assertions:

```rust
assert!(x.contains(r#"encoding="UTF-16""#), "schtasks requires UTF-16");
assert!(x.contains("<UserId>"), "logon trigger must be user-scoped or it needs admin");
```

Note `windows_task_xml` is compiled on all platforms (only its *caller* is
Windows-gated), so these run in CI on Linux.

## Verification

The whole path was confirmed working on Windows: with the UTF-16 declaration +
BOM and a user-scoped `<LogonTrigger>`, `schtasks /Create /TN ... /XML ... /F`
returned `SUCCESS` **unelevated**. All scratch tasks created during
investigation were deleted; no residue.

To re-verify after fixing, on the Windows box:

```powershell
cc-screen-rust install --hub https://ccscreen.dibbla.work --machine-id harebell-win --hub-only --enroll
schtasks /Query /TN cc-screen-rust /V /FO LIST   # should list the task, not error
```

## Not affected

`crates/hub/src/service.rs:189` looks similar but is a **launchd plist**, not a
Windows task — it needs no change. The hub crate contains no `schtasks` or
`LogonTrigger` code at all. The UTF-8 declaration is correct there.
