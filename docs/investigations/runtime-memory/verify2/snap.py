import sys

def rollup(pid):
    d = {}
    with open(f"/proc/{pid}/smaps_rollup") as f:
        for line in f:
            if ':' in line:
                k, v = line.split(':', 1)
                v = v.strip()
                if v.endswith('kB'):
                    d[k] = int(v[:-2].strip())
    d['USS'] = d.get('Private_Clean', 0) + d.get('Private_Dirty', 0)
    return d

if __name__ == "__main__":
    pids = [int(x) for x in sys.argv[1:]]
    agg = {'Rss': 0, 'Pss': 0, 'USS': 0, 'Shared_Clean': 0, 'Shared_Dirty': 0,
           'Private_Clean': 0, 'Private_Dirty': 0}
    for pid in pids:
        d = rollup(pid)
        print(f"pid {pid}: RSS {d.get('Rss',0)} PSS {d.get('Pss',0)} USS {d['USS']} "
              f"Shared_Clean {d.get('Shared_Clean',0)} Shared_Dirty {d.get('Shared_Dirty',0)} "
              f"Priv_Dirty {d.get('Private_Dirty',0)}")
        for k in agg:
            agg[k] += d.get(k, 0)
    print(f"AGGREGATE over {len(pids)} pids: RSS {agg['Rss']} PSS {agg['Pss']} USS {agg['USS']} "
          f"Shared_Clean {agg['Shared_Clean']} Shared_Dirty {agg['Shared_Dirty']}")
