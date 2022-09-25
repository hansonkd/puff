import contextvars
import dataclasses
from typing import Any

from flask import Flask

import puff
import redis

SAMPLE = 10

# app = Flask(__name__)
r = redis.Redis(host='localhost')

puff_redis = puff.get_redis()
#
# @app.route("/puff/")
# def hello_world_puff():
#     puff_redis.set("blam", "ok")
#     gets = []
#     for _ in range(SAMPLE):
#         gets.append(puff_redis.get("blam"))
#     return "".join(gets)
#
#
# @app.route("/")
# def hello_world():
#     r.set("blam", "ok")
#     gets = []
#     for _ in range(SAMPLE):
#         gets.append(r.get("blam"))
#     return b"".join(gets)
#

def app(environ, start_response):
    start_response("200 OK", [])
    puff_redis.set("blam", "ok")
    gets = []
    for _ in range(SAMPLE):
        gets.append(puff_redis.get("blam"))
    st = "".join(gets)
    return [st.encode("utf8")]
#
#
# def do_work(q, return_result):
#     return_result(q)
#
#
# def redis_get(q):
#     return wrap_async(lambda rr: do_work(q, rr))
#
#
# def my_job():
#     r = redis_get("blam")
#     print(f"Got {r} from redis.")
#     r = redis_get("blam2")
#     print(f"Got {r} from redis again.")
#
#
# t = start_event_loop()
#
#
# def returner(r):
#     print(f"Got {r} as a value.")
#
#
# t.spawn(my_job, [], {}, returner)
# t.spawn(my_job, [], {}, returner)
# t.spawn(my_job, [], {}, returner)
# t.spawn(my_job, [], {}, returner)