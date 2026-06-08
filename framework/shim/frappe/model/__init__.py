"""frappe.model — package marker. Fieldtype groupings some controllers import."""

# A few constants controllers occasionally import from frappe.model
default_fields = (
    "doctype", "name", "owner", "creation", "modified", "modified_by",
    "docstatus", "idx", "parent", "parentfield", "parenttype",
)

no_value_fields = (
    "Section Break", "Column Break", "Tab Break", "HTML", "Heading", "Button",
    "Fold", "Table", "Table MultiSelect",
)

table_fields = ("Table", "Table MultiSelect")

child_table_fields = ("Table", "Table MultiSelect")

numeric_fieldtypes = ("Currency", "Int", "Long Int", "Float", "Percent", "Check", "Rate")

data_fieldtypes = (
    "Currency", "Int", "Long Int", "Float", "Percent", "Check", "Small Text",
    "Long Text", "Code", "Text Editor", "Markdown Editor", "HTML Editor", "Date",
    "Datetime", "Time", "Text", "Data", "Link", "Dynamic Link", "Password", "Select",
    "Rating", "Read Only", "Attach", "Attach Image", "Signature", "Color", "Barcode",
    "Geolocation", "Duration", "Icon", "Phone", "Autocomplete", "JSON",
)

optional_fields = ("_user_tags", "_comments", "_assign", "_liked_by", "_seen")

display_fieldtypes = ("Section Break", "Column Break", "Tab Break", "HTML", "Heading", "Button", "Fold")
