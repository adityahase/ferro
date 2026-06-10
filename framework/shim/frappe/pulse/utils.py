"""frappe.pulse.utils — version helper used by apps (e.g. crm.utils.is_frappe_version) at import
time. Returns the framework version as a real numeric string so version gates evaluate correctly."""

import frappe


def get_frappe_version():
	return frappe.__version__
