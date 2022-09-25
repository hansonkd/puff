import contextvars
import dataclasses
from typing import Any

from flask import Flask

import puff
import redis

SAMPLE = 10

app = Flask(__name__)
r = redis.Redis(host='localhost')

puff_redis = puff.get_redis()

@app.route("/puff/")
def hello_world_puff():
    redis_set("blam", "ok").join()
    gets = []
    for _ in range(SAMPLE):
        gets.append(redis_get("blam"))
    return b"".join(puff.join_all(gets))


@app.route("/concat/")
def hello_world_concat():
    redis_set("blam", "ok").join()
    return redis_get_ten("blam").join()



@app.route("/")
def hello_world():
    r.set("blam", "ok")
    gets = []
    for _ in range(SAMPLE):
        gets.append(r.get("blam"))
    return b"".join(gets)


from puff import wrap_async


def redis_get(q):
    return wrap_async(lambda ctx, rr: puff_redis.get(ctx, rr, q), join=False)


def redis_get_ten(q):
    return wrap_async(lambda ctx, rr: puff_redis.get_ten(ctx, rr, q), join=False)


def redis_set(k, v):
    return wrap_async(lambda ctx, rr: puff_redis.set(ctx, rr, k, v), join=False)


# def app(environ, start_response):
#     start_response("200 OK", [])
#     redis_set("blam", "ok")
#     gets = []
#     for _ in range(SAMPLE):
#         gets.append(redis_get("blam"))
#     return [b"".join(gets)]
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