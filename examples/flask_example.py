from flask import Flask

app = Flask(__name__)

@app.route("/")
def hello_world():
    return "<p>Hello, World!</p>"


# def app(environ, start_response):
#     start_response("200 OK", [])
#     return [b"<p>Hello, World!</p>"]