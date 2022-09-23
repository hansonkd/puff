from flask import Flask

import puff
import redis

app = Flask(__name__)
r = redis.Redis(host='localhost')

puff_redis = puff.get_redis();

@app.route("/puff/")
def hello_world_puff():
    puff_redis.set("blam", "ok")
    return puff_redis.get("blam")


@app.route("/")
def hello_world():
    r.set("blam", "ok")
    return r.get("blam")


# def app(environ, start_response):
#     start_response("200 OK", [])
#     return [b"<p>Hello, World!</p>"]