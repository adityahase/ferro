# frappe-core seed

`core.db.gz` is a **pristine Frappe core SQLite database** (carved from the `develop` /
`17.0.0-dev` branch — the only branch that supports a SQLite site) — the 278 framework
("Core", "Desk", "Website", "Contacts", …) DocTypes, their metadata
(`tabDocType`/`tabDocField`/`tabDocPerm`), the `Administrator` + `Guest` users, the
standard roles, and `tabSingles`/`tabSeries`/`__Auth` — with **no app tables and no
data rows**. It is the faithful baseline that `ferro new-site` decompresses into a new
site; `ferro install-app` then layers each app's schema on top (see
`framework/build_db.py`).

It was carved from a real `bench new-site --db-type sqlite` database (every column name
matches what the Rust runtime queries), then stripped of the five demo apps and all
demo rows, and `VACUUM`ed. `Administrator` carries a usable `api_key`; the matching
`api_secret` is Fernet-encrypted with the site `encryption_key` written into
`site_config.json` at `new-site` time.

Regenerate or inspect: `python3 framework/build_db.py --help`.
