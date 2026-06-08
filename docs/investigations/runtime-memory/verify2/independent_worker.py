import os, signal, sys

# Warm a single independent worker, print PID, then block.
import frappe
frappe.init(site='mysite.sqlite')
frappe.connect()
frappe.get_meta('User')
frappe.get_meta('DocType')

pidfile = sys.argv[1]
with open(pidfile, 'w') as f:
    f.write(str(os.getpid()))
sys.stderr.write(f"WARM worker pid={os.getpid()}\n")
sys.stderr.flush()
signal.pause()
