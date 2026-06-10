"""frappe.desk.page.setup_wizard.setup_wizard — the bits app setup code imports. ERPNext's
install_fixtures imports make_records from here; without a real implementation it resolved to a
no-op stub and setup created nothing (Fiscal Year / Company / Price Lists silently skipped)."""

import frappe


def make_records(records, debug=False):
    """Insert each fixture record, tolerating duplicates / per-record failures (real Frappe wraps
    each in a savepoint and shows-but-continues on error). Best-effort, like the original."""
    for record in records:
        condition = record.get("__condition") if isinstance(record, dict) else None
        if callable(condition) and not condition():
            continue
        doctype = record.get("doctype")
        if not doctype:
            continue
        try:
            doc = frappe.get_doc(dict(record))
            # root tree nodes have no parent — don't fail their mandatory parent link
            try:
                doc.flags.ignore_mandatory = True
            except Exception:
                pass
            doc.insert(ignore_permissions=True, ignore_if_duplicate=True)
        except frappe.exceptions.NameError:
            pass  # already exists
        except Exception as e:
            if frappe.__dict__.get("flags") and getattr(frappe.flags, "in_test", False):
                raise
            import os
            if os.environ.get("FERRO_TRACE"):
                import traceback
                traceback.print_exc()
            # otherwise: show-and-continue (matches real make_records' resilience)
