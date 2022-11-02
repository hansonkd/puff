import contextvars
import dataclasses
from typing import Any

from flask import Flask

import puff
from puff import wrap_async
import puff.postgres
from puff.redis import global_redis
import redis

SAMPLE = 1000

app = Flask(__name__)
r = redis.Redis(host="localhost")


@app.route("/pg/")
def hello_world_pg():
    postgres_conn = puff.postgres.PostgresConnection()
    cursor = postgres_conn.cursor()
    cursor.execute("SELECT '[1,2,3]'::jsonb")
    results = cursor.fetchall()
    return str(results)


@app.route("/deep/")
def hello_world_puff():
    redis_set("blam", "ok").join()
    gets = []
    for _ in range(SAMPLE):
        gets.append(redis_get("blam"))
    return b"".join(puff.join_all(gets))


@app.route("/deeper/")
def hello_world_concat():
    redis_set("blam", "ok").join()
    return redis_get_many("blam", SAMPLE).join()


@puff.blocking
@app.route("/shallow/")
def hello_world():
    r.set("blam", "ok")
    gets = []
    for _ in range(SAMPLE):
        gets.append(r.get("blam"))
    return b"".join(gets)


def redis_get(q):
    return global_redis.get(q)


def redis_get_many(q, l):
    return wrap_async(
        lambda rr: puff.global_state().concat_redis_gets(rr, q, l), join=False
    )


def redis_set(k, v):
    return global_redis.set(k, v)
