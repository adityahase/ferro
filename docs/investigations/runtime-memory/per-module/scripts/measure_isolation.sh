#!/usr/bin/env bash
# Standalone (isolation) memory cost of each module, measured the SAME way as the
# existing baseline harness: peak RSS (ru_maxrss) via `/usr/bin/time -f '%M'`, in KiB,
# min-of-N over fresh processes. Captures EVERYTHING in RSS, including native/C-ext
# memory (orjson's Rust heap, OpenSSL, libxml2, Pillow, etc.) that tracemalloc misses.
#
# "Isolation" = a fresh interpreter that imports ONLY this module (plus whatever it
# transitively pulls). Because shared transitive deps are re-counted by every module
# that pulls them, the column does NOT sum to `import frappe` -- it answers
# "if a worker imported only this, what would it cost?", which is the right question
# for deciding what to make lazy.
set -u
N=${N:-3}
GTIME=/usr/bin/time
PY=/home/frappe/benches/bench-cpython314/env/bin/python
BENCH=/home/frappe/benches/bench-cpython314

# label|import statement  (import names, not PyPI names; representative submodules for
# packages whose cost is in a submodule rather than the top package)
MODULES=(
  "bare|pass"
  # --- Frappe's first two third-party imports (in __init__.py order) ---
  "orjson|import orjson"
  "werkzeug|from werkzeug.datastructures import Headers"
  # --- templating / serialization ---
  "jinja2|import jinja2"
  "markupsafe|import markupsafe"
  "pyyaml|import yaml"
  # --- data validation / typing ---
  "pydantic|import pydantic"
  "pydantic_core|import pydantic_core"
  "typing_extensions|import typing_extensions"
  # --- crypto / security (native: OpenSSL) ---
  "cryptography|from cryptography.hazmat.primitives.asymmetric import rsa"
  "pyopenssl|from OpenSSL import SSL"
  "passlib|from passlib.context import CryptContext"
  "pyjwt|import jwt"
  "rsa|import rsa"
  "pyotp|import pyotp"
  "ldap3|import ldap3"
  "oauthlib|import oauthlib"
  "zxcvbn|import zxcvbn"
  # --- XML / HTML / parsing (native: libxml2/libxslt) ---
  "lxml|import lxml.etree"
  "bs4|import bs4"
  "html5lib|import html5lib"
  "cssutils|import cssutils"
  "premailer|import premailer"
  "markdown2|import markdown2"
  "markdownify|import markdownify"
  "nh3|import nh3"
  # --- imaging (native: libjpeg/zlib/...) ---
  "pillow|from PIL import Image"
  # --- db drivers / sql ---
  "pymysql|import pymysql"
  "mysqlclient|import MySQLdb"
  "psycopg2|import psycopg2"
  "sqlparse|import sqlparse"
  "sql_metadata|import sql_metadata"
  "pypika|import pypika"
  # --- queue / cache / net ---
  "redis|import redis"
  "hiredis|import hiredis"
  "rq|import rq"
  "requests|import requests"
  "urllib3|import urllib3"
  "websockets|import websockets"
  # --- big data-table modules (classic memory hogs) ---
  "babel|import babel"
  "pytz|import pytz"
  "dateutil|from dateutil import parser"
  "faker|from faker import Faker; Faker()"
  "phonenumbers|import phonenumbers"
  "pycountry|import pycountry"
  "num2words|import num2words"
  "croniter|import croniter"
  # --- office / docs ---
  "openpyxl|import openpyxl"
  "pypdf|import pypdf"
  "xlsxwriter|import xlsxwriter"
  "xlrd|import xlrd"
  # --- google / integrations ---
  "googleapiclient|from googleapiclient.discovery import build"
  "google_auth|import google.auth"
  "requests_oauthlib|import requests_oauthlib"
  # --- observability ---
  "sentry_sdk|import sentry_sdk"
  "posthog|import posthog"
  # --- dev / repl (eagerly importable, heavy) ---
  "ipython|import IPython"
  # --- sandboxing ---
  "restrictedpython|import RestrictedPython"
  # --- misc ---
  "semantic_version|import semantic_version"
  "tenacity|import tenacity"
  "gitpython|import git"
  "psutil|import psutil"
  # --- the anchor: the whole framework ---
  "import_frappe|import frappe"
)

measure() { # $1=stmt -> min peak rss KiB over N runs, or NA
  local stmt="$1" best="" v
  for _ in $(seq "$N"); do
    v=$(cd "$BENCH" && "$GTIME" -f '%M' "$PY" -c "$stmt" 2>/tmp/_iso_err >/dev/null; tail -1 /tmp/_iso_err 2>/dev/null)
    case "$v" in (''|*[!0-9]*) continue;; esac
    if [ -z "$best" ] || [ "$v" -lt "$best" ]; then best="$v"; fi
  done
  [ -z "$best" ] && echo "NA" || echo "$best"
}

echo "module,import_stmt,peak_rss_kib_min_of_${N},status"
for row in "${MODULES[@]}"; do
  IFS='|' read -r label stmt <<<"$row"
  # status: does it import at all? (separate quick run capturing rc)
  if (cd "$BENCH" && "$PY" -c "$stmt" >/dev/null 2>/tmp/_iso_rc); then st=ok; else st=FAILED; fi
  kib=$(measure "$stmt")
  # escape comma in stmt for CSV
  safe_stmt=${stmt//,/;}
  echo "${label},${safe_stmt},${kib},${st}"
done
