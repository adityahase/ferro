import os, signal, sys, time

# gunicorn --preload model:
# parent imports + warms frappe ONCE, then forks N children that do work.
import frappe
frappe.init(site='mysite.sqlite')
frappe.connect()
frappe.get_meta('User')
frappe.get_meta('DocType')

N = int(sys.argv[1])
workdir = sys.argv[2]
mode = sys.argv[3] if len(sys.argv) > 3 else 'idle'  # idle | read | loop
do_freeze = (len(sys.argv) > 4 and sys.argv[4] == 'freeze')

if do_freeze:
    import gc
    gc.collect()
    gc.freeze()
    sys.stderr.write("gc.freeze() done\n")

# Close DB connection in parent BEFORE forking so children open their own.
frappe.db.close()

parent_pid = os.getpid()
children = []
for i in range(N):
    pid = os.fork()
    if pid == 0:
        # child
        cpid = os.getpid()
        # children need their own DB connection
        if mode in ('read', 'loop'):
            frappe.connect()
            if mode == 'read':
                # one request-like read
                frappe.get_all('User', fields=['name'], limit=5)
                frappe.get_meta('User')
                frappe.get_meta('DocType')
            elif mode == 'loop':
                # hammer it: repeated get_meta + get_all to maximize refcount churn
                for _ in range(50):
                    frappe.clear_cache()
                    frappe.get_meta('User')
                    frappe.get_meta('DocType')
                    frappe.get_meta('DocField')
                    frappe.get_all('DocType', fields=['name', 'module'], limit=200)
                    frappe.get_all('User', fields=['name', 'email'], limit=50)
        with open(f"{workdir}/pid_child_{i}.pid", 'w') as f:
            f.write(str(cpid) + "\n")
        sys.stderr.write(f"child {i} pid={cpid} done mode={mode}\n")
        sys.stderr.flush()
        signal.pause()
        os._exit(0)
    else:
        children.append(pid)

with open(f"{workdir}/pid_parent.pid", 'w') as f:
    f.write(str(parent_pid) + "\n")
sys.stderr.write(f"parent pid={parent_pid} forked {N} children: {children}\n")
sys.stderr.flush()
signal.pause()
