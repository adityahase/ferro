"""frappe.exceptions — the subset of Frappe's exception hierarchy app controllers raise/catch."""


class FrappeException(Exception):
    http_status_code = 500


class ValidationError(FrappeException):
    http_status_code = 417


class MandatoryError(ValidationError):
    pass


class PermissionError(FrappeException):
    http_status_code = 403


class AuthenticationError(FrappeException):
    http_status_code = 401


class DoesNotExistError(ValidationError):
    http_status_code = 404


class DuplicateEntryError(ValidationError):
    http_status_code = 409


class LinkExistsError(ValidationError):
    pass


class LinkValidationError(ValidationError):
    pass


class NameError(FrappeException):
    pass


class InvalidStatusError(ValidationError):
    pass


class UniqueValidationError(ValidationError):
    pass


class TimestampMismatchError(ValidationError):
    pass


class CharacterLengthExceededError(ValidationError):
    pass


class QueryDeadlockError(FrappeException):
    pass


class QueryTimeoutError(FrappeException):
    pass


_DYNAMIC = {}


def __getattr__(name):
    # any other *Error the apps import/catch -> a real FrappeException subclass (so `except X` works)
    if name.startswith("__") and name.endswith("__"):
        raise AttributeError(name)
    cls = _DYNAMIC.get(name)
    if cls is None:
        cls = type(name, (FrappeException,), {})
        _DYNAMIC[name] = cls
    return cls
