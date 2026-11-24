"""Synthetic disfluency injection over permissively licensed clean text.

Clean sources: OpenAssistant oasst1 prompter messages (Apache-2.0), Google Synthetic-Persona-Chat
(CC-BY-4.0), and hand-written terminal/agent templates. Output rows: {"words": [...], "labels":
[0 keep / 1 delete]}.
"""

import csv
import gzip
import json
import random
import re
import sys

random.seed(int(sys.argv[3]) if len(sys.argv) > 3 else 13)
DL = sys.argv[1]
OUT = sys.argv[2]

FILLERS = ["um", "uh", "uhm", "er", "eh", "hmm", "ah", "erm", "mm", "uh", "um", "um", "uh"]
EDIT_TERMS = ["I mean", "sorry", "no", "uh", "um", "wait", "or rather", "I mean,", "actually no",
              "no no", "uh, I mean"]
DISCOURSE_MID = ["you know", "like", "I mean", "kind of like", "like, you know"]
DISCOURSE_START = ["so", "so", "well", "I mean", "you know", "okay so", "so like", "like",
                   "so yeah", "so basically"]


def sentences_oasst():
    out = []
    with gzip.open(f"{DL}/oasst_prompts.jsonl.gz", "rt") as f:
        for line in f:
            m = json.loads(line)
            if m.get("lang") != "en" or m.get("role") != "prompter":
                continue
            text = m["text"]
            if "```" in text or "\n\n" in text and len(text) > 600:
                continue
            for s in re.split(r"(?<=[.?!])\s+|\n+", text):
                s = s.strip()
                n = len(s.split())
                if 3 <= n <= 30 and not re.search(r"[{}<>\[\]|`\\]", s):
                    out.append(s)
    return out


def sentences_spc():
    out = []
    with open(f"{DL}/spc_train.csv", newline="") as f:
        for row in csv.DictReader(f):
            for line in row["Best Generated Conversation"].split("\n"):
                line = re.sub(r"^User \d:\s*", "", line).strip()
                if "[" in line:
                    continue
                for s in re.split(r"(?<=[.?!])\s+", line):
                    n = len(s.split())
                    if 3 <= n <= 30:
                        out.append(s)
    return out


SLOTS = {
    "file": ["main.rs", "index.ts", "package.json", "Cargo.toml", "the readme", "config.yaml",
             "src/lib.rs", "app.py", "the Dockerfile", "the makefile", ".env", "utils.go",
             "server.js", "the test file", "setup.py", "docker-compose.yml", "build.gradle"],
    "dir": ["this directory", "the src folder", "the home directory", "the parent directory",
            "the logs folder", "/tmp", "the repo root", "the docs folder", "~/projects"],
    "branch": ["main", "master", "develop", "the feature branch", "release", "origin main",
               "my branch", "the hotfix branch"],
    "tool": ["git", "docker", "cargo", "npm", "kubectl", "python", "pip", "brew", "make",
             "terraform", "yarn", "go", "node", "ssh", "curl"],
    "thing": ["bug", "test", "import", "type error", "warning", "lint error", "memory leak",
              "race condition", "null pointer", "typo", "timeout", "deadlock", "crash"],
    "n": ["two", "three", "five", "ten", "20", "100", "the last", "a few", "all the"],
    "port": ["3000", "8080", "5432", "443", "8000", "9090"],
    "flag": ["dash r", "--force", "-rf", "dash dash verbose", "-la", "--help", "-m", "the dry run"],
}

TEMPLATES = [
    "show me the git log for {n} commits", "run the tests in {dir}", "list all files in {dir}",
    "what does the {flag} flag do", "open {file} and fix the {thing}", 
    "find all the files larger than {n} megabytes", "explain this error", "create a new branch called {branch}",
    "switch to {branch}", "merge {branch} into {branch}", "check out {branch}", "pull the latest changes",
    "why is the {thing} happening", "fix the {thing} in {file}", "how do I install {tool}",
    "update {tool} to the latest version", "show the diff for {file}", "what changed in {file}",
    "delete {file}", "move {file} to {dir}", "copy {file} into {dir}", "rename {file}",
    "can you summarize {file}", "what's in {dir}", "how much disk space is left",
    "restart the server on port {port}", "stop all the docker containers", "run {tool} build",
    "commit everything with a message", "push my changes to {branch}", "revert the last commit",
    "show me what's running on port {port}", "add a test for the {thing}", "write a script that cleans {dir}",
    "go to {dir}", "cd into {dir}", "print the environment variables", "set up a virtual environment",
    "how do I use {tool}", "read {file} and tell me what it does",
    "search for the {thing} in {dir}", "count the lines in {file}", "compress {dir} into a zip file",
    "check if {tool} is installed", "make the tests pass", "lint the code in {dir}",
    "can you look at the {thing} in {file}", "I want to see {n} lines of the log",
    "tail the logs", "show me the open pull requests", "squash {n} commits", "rebase onto {branch}",
    "help me debug the {thing}", "use {tool} to deploy",
    "format {file}", "open the file {file}", "edit the config file {file}", "start the dev server",
    "grep for the word {thing} in {dir}", "stash my changes and switch to {branch}", "build it in release mode",
    "ssh into the staging server", "show me the memory usage", "check the error logs", "read the readme", "find where the function is called", "generate a changelog", 
    "run it with the {flag} flag", "add {file} to the gitignore", "clean up the unused imports",
]


def fill(t):
    return re.sub(r"\{(\w+)\}", lambda m: random.choice(SLOTS[m.group(1)]), t)


def templated():
    s = fill(random.choice(TEMPLATES))
    if random.random() < 0.4:
        s = random.choice(["can you ", "please ", "I want to ", "I need to ", "could you ", "let's ",
                           "help me ", "I'd like to ", "go ahead and ", "try to "]) + s
    s = s[0].upper() + s[1:]
    end = "?" if re.match(r"(?i)(can|could|what|why|how|where|is|do)", s) else "."
    return s + end


# Fluent sentences that use filler-homographs, which must stay.
KEEP_TEMPLATES = [
    "Do you know where I left {file}?", "Do you know how to fix the {thing}?", "I'd like to open {file}.",
    "I would like to {verb} {file}.", "I like the way {file} looks.", "Can it look more like {file}?",
    "Add a check so that the {thing} goes away.", "It is so slow when I open {file}.",
    "I mean it, delete {file}.", "So far so good.", "You know the drill.", "Well done.",
    "Do you know whether {tool} is installed?", "Things like {tool} and {tool} should work.",
    "It feels like a {thing}.", "Something like that.", "So that is why the {thing} happens.",
    "What do you mean by {thing}?", "I know that that {thing} is real.", "Run {tool} {flag}.",
    "Echo hello into {file}.", "Is it like this or like that?", "Do you know what {flag} does?",
    "I see what you mean.", "Run npm run {script}.", "Run yarn run {script} in {dir}.",
    "Echo {w} {w} into {file}.", "Type {w} {w} and press enter.", "I mean what I wrote.",
    "You know the {thing} I told you about?", "Open the file {file}.", "Start the dev server.",
    "Grep for the word {w}.", "Stash my changes and pull.", "Do you know the password?",
    "That is so cool.", "Delete the build folder.", "The file is called {file}.", "Show me the test file.",
    "Run make make-docs.", "Say it again, louder.", "Well, it works now.", "Okay, so what now?",
    "I mean, it is fine.", "Set it up with brew install {w}.", "Run cargo test on the {w} crate.",
    "Deploy it with {tool} deploy.", "Build it using npm run build.", "Format it with cargo fmt.",
    "Push it with git push origin {branch}.", "Create it with mkdir {w}.", "Install {w} using pip install {w}.",
    "Run {tool} check on the {dir}.", "Check it with git status.", "Test it with cargo test on {branch}.",
    "Add it with git add {file}.", "Open it with vim {file}.", "Start it with docker start {w}.",
    "Remove it with rm {file}.", "List them with ls -la.", "Clean it with rm -rf {dir}.", "Like I said, it is broken.", "So, what do you think?", "Why is it so hard to use {tool}?", "It looks like the {thing} is gone.",
]
WORDS = ["hello", "foo", "bar", "test", "yes", "no", "bye", "ok", "done", "hi"]
SCRIPTS = ["build", "dev", "test", "lint", "start", "format"]
VERBS = ["open", "read", "edit", "check", "delete", "format"]


def keep_template():
    t = random.choice(KEEP_TEMPLATES)
    return re.sub(r"\{(\w+)\}", lambda m: random.choice({"verb": VERBS, "w": WORDS, "script": SCRIPTS}.get(m.group(1)) or SLOTS[m.group(1)]), t)


def strip(w):
    return re.sub(r"^[^\w]+|[^\w]+$", "", w)


def cap(w):
    return w[:1].upper() + w[1:]


class Builder:
    """Builds the noisy word list from clean words with insertions."""

    def __init__(self, words):
        self.words = list(words)
        self.labels = [0] * len(words)

    def insert(self, i, new_words, label=1):
        self.words[i:i] = new_words
        self.labels[i:i] = [label] * len(new_words)


def comma(w):
    return w if w.endswith(",") else w + ","


def apply_ops(words):
    b = Builder(words)
    n_ops = random.choices([1, 2, 3, 4], weights=[45, 33, 15, 7])[0]
    for _ in range(n_ops):
        op = random.choices(
            ["filler", "repeat", "stutter", "repair", "abandon", "discourse"],
            weights=[30, 20, 12, 15, 10, 13])[0]
        # Positions only among clean words' starts (kept tokens) to avoid nesting chaos.
        keep_idx = [i for i, l in enumerate(b.labels) if l == 0]
        if not keep_idx:
            break
        i = random.choice(keep_idx + [keep_idx[0]] * 2)
        if op == "filler":
            f = random.choice(FILLERS)
            k = random.choices([1, 2], weights=[85, 15])[0]
            fs = [random.choice(FILLERS) for _ in range(k)]
            style = random.random()
            if style < 0.5:
                fs[-1] = comma(fs[-1])
            if i > 0 and style < 0.35 and b.labels[i - 1] == 0 and not re.search(r"[.?!,]$", b.words[i - 1]):
                b.words[i - 1] = b.words[i - 1] + ","
            b.insert(i, fs)
        elif op == "repeat":
            k = random.choices([1, 2, 3], weights=[55, 30, 15])[0]
            span = [w for w, l in zip(b.words[i:i + k], b.labels[i:i + k]) if True]
            if any(l for l in b.labels[i:i + k]):
                continue
            rep = [strip(w) or w for w in span]
            if i == 0:
                rep[0] = span[0]
            if random.random() < 0.4:
                rep[-1] = comma(rep[-1])
            times = random.choices([1, 2], weights=[90, 10])[0]
            extra = [comma(random.choice(FILLERS))] if random.random() < 0.15 else []
            b.insert(i, rep * times + extra)
            if i == 0 and b.words[i + len(rep) * times + len(extra)][:1].isupper():
                pass
        elif op == "stutter":
            w = strip(b.words[i])
            if len(w) < 3 or not w.isalpha():
                continue
            j = random.randint(1, min(4, len(w) - 1))
            frag = w[:j]
            if random.random() < 0.3:
                frag += "-"
            times = random.choices([1, 2], weights=[85, 15])[0]
            b.insert(i, [frag] * times)
        elif op == "repair":
            # Substitution repair: a copy of the next 1-3 words with the last swapped out.
            k = random.choices([1, 2, 3], weights=[40, 40, 20])[0]
            span = b.words[i:i + k]
            if len(span) < 1 or any(b.labels[i:i + k]):
                continue
            rep = [strip(w) or w for w in span]
            if i == 0:
                rep[0] = span[0]
            other = strip(random.choice(POOL_WORDS))
            if not other:
                continue
            rep[-1] = other if random.random() < 0.8 else other[: random.randint(1, max(1, len(other) - 1))]
            r = random.random()
            if r < 0.45:
                tail = [comma(rep.pop())] if rep else []
                ed = random.choice(EDIT_TERMS).split()
                ed[-1] = comma(ed[-1])
                rep = rep + tail + ed
            elif r < 0.8:
                rep[-1] = comma(rep[-1])
            b.insert(i, rep)
        elif op == "abandon":
            # A false start: the opening of another request, then a restart.
            other = random.choice(ABANDON_SOURCES).split()
            k = random.randint(1, min(4, len(other)))
            frag = [w.strip(".?!") for w in other[:k]]
            if i != 0:
                frag[0] = frag[0].lower() if frag[0] != "I" else frag[0]
            r = random.random()
            if r < 0.4:
                frag[-1] = comma(frag[-1])
                ed = random.choice(EDIT_TERMS + FILLERS).split()
                ed[-1] = comma(ed[-1])
                frag += ed
            elif r < 0.8:
                frag[-1] = comma(frag[-1])
            b.insert(i, frag)
        elif op == "discourse":
            if i == 0 or random.random() < 0.3:
                d = random.choice(DISCOURSE_START).split()
                d[0] = cap(d[0])
                if random.random() < 0.6:
                    d[-1] = comma(d[-1])
                if b.labels and b.labels[0] == 0 and b.words[0] != "I" and random.random() < 0.7:
                    b.words[0] = b.words[0][:1].lower() + b.words[0][1:]
                b.insert(0, d)
            else:
                d = random.choice(DISCOURSE_MID).split()
                if random.random() < 0.6:
                    d[-1] = comma(d[-1])
                    if b.labels[i - 1] == 0 and not re.search(r"[.?!,]$", b.words[i - 1]):
                        b.words[i - 1] += ","
                b.insert(i, d)
    # Sentence-initial deletions: the clean text's first word was capitalized; mimic STT where the
    # first spoken word is capitalized and the rest of the original first word lowercased.
    if b.labels and b.labels[0] == 1:
        b.words[0] = cap(b.words[0])
        first_keep = next((i for i, l in enumerate(b.labels) if l == 0), None)
        if first_keep is not None and b.words[first_keep] != "I" and random.random() < 0.7:
            w = b.words[first_keep]
            if not (len(w) > 1 and w[1:2].isupper()):
                b.words[first_keep] = w[:1].lower() + w[1:]
    return b


def stt_style(words):
    """Some rows lowercase and unpunctuated, like raw ASR."""
    r = random.random()
    if r < 0.2:
        return [re.sub(r"[.,?!;:]+$", "", w).lower() or w for w in words]
    if r < 0.3:
        return [re.sub(r",$", "", w) for w in words]
    return words


oasst = sentences_oasst()
spc = sentences_spc()
random.shuffle(oasst)
random.shuffle(spc)
print("oasst", len(oasst), "spc", len(spc), file=sys.stderr)
POOL_WORDS = [w for s in spc[:5000] for w in s.split() if strip(w).isalpha()]
ABANDON_SOURCES = [templated() for _ in range(3000)] + spc[:3000] + oasst[:3000]

N = int(sys.argv[4]) if len(sys.argv) > 4 else 150000
rows = []
for _ in range(N):
    r = random.random()
    if r < 0.38:
        s = random.choice(oasst)
    elif r < 0.68:
        s = random.choice(spc)
    elif r < 0.93:
        s = templated()
    else:
        s = keep_template()
    words = s.split()
    if random.random() < 0.2:
        b = Builder(words)  # fluent control
    else:
        b = apply_ops(words)
    noisy = stt_style(b.words)
    rows.append({"words": noisy, "labels": b.labels})

with open(OUT, "w") as f:
    for row in rows:
        f.write(json.dumps(row) + "\n")
print("wrote", len(rows), file=sys.stderr)
