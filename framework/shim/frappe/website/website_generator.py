"""frappe.website.website_generator — minimal WebsiteGenerator base (a real Document subclass)
so doctypes like `class HDArticle(WebsiteGenerator)` register and run their own controller logic.
The web-route/sitemap machinery is out of scope (deferred), but the doctype's own methods run."""
from frappe.model.document import Document


class WebsiteGenerator(Document):
    def get_context(self, context):
        return context

    def on_update(self):
        pass

    def on_trash(self):
        pass

    def validate(self):
        pass

    def get_route(self):
        return self.get("route") or ""
