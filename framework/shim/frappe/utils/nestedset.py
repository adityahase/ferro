"""frappe.utils.nestedset — minimal NestedSet base so tree doctypes register & run their own logic.

The real NestedSet maintains lft/rgt tree bounds in on_update/on_trash. We provide a Document
subclass whose tree-maintenance hooks are no-ops (disclosed), so a controller like
`class Territory(NestedSet)` IS a real Document subclass — it registers, and its OWN
validate/on_update/etc. execute. The lft/rgt bookkeeping is deferred (out of REST scope).
"""
from frappe.model.document import Document


class NestedSet(Document):
    nsm_parent_field = "parent"

    def on_update(self):
        pass  # real NestedSet updates lft/rgt here; deferred

    def on_trash(self, allow_root_deletion=False):
        pass

    def before_rename(self, *a, **k):
        pass

    def after_rename(self, *a, **k):
        pass

    def validate_one_root(self):
        pass

    def set_nsm_root(self):
        pass


def update_nsm(doc):
    return None


def rebuild_tree(*a, **k):
    return None


def get_root_of(doctype, **k):
    return None


def get_ancestors_of(doctype, name, **k):
    return []


def get_descendants_of(doctype, name, **k):
    return []


class NestedSetRecursionError(Exception):
    pass


class NestedSetMultipleRootsError(Exception):
    pass


class NestedSetChildExistsError(Exception):
    pass


class NestedSetInvalidMergeError(Exception):
    pass
