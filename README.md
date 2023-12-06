# Rama

Rama is a port of [llama.c](ttps://github.com/karpathy/llama2.c) in Rust, some of the code referenced another port [here](https://github.com/leo-du/llama2.rs) from [leo-du](https://github.com/leo-du).

This repo was created to learn Rust and understand llama2 model architectures by code - debugging is quite challenging tbh. The repo is also annotated with learning materials and documentations.

Plan is to catch up with the performance of llama.cpp.

## Usage
```
git clone https://github.com/oliverhu/rama
wget https://huggingface.co/karpathy/tinyllamas/resolve/main/stories15M.bin
cargo build --release
cargo run --release -- -m stories15M.bin -t tokenizer.bin -p 'once upon a time'
```
(Note release build is about 10x faster...for my M1 Mac, debug build gives ~35 tok/s,
release build gives 370 tok/s)

For llama2 model from Meta:
```
pip install -r export/requirements.txt
python export/export.py llama2_7b.bin --meta-llama path/to/llama/model/7B
cargo run --release -- -m llama2_7b.bin -t tokenizer.bin -p 'once upon a time'
```

Sample output:
```
❯ cargo run --release -- -m llama2-7b.bin -t tokenizer.bin -p 'once upon a time' -r 0.5

    Finished release [optimized] target(s) in 0.01s
    Running `target/release/rama -m llama2-7b.bin -t tokenizer.bin -p 'once upon a time' -r 0.5`

once upon a time, there was a little boy who lived in a little town. He was a very good boy. He loved his family, his friends, and his little town..
One day, the little boy was walking down the street when he saw a beautiful little house. He had never seen a house like it before. It was so beautiful that he wanted to live in it..
The little boy walked up to the house and knocked on the door. A beautiful lady answered the door. She was the owner of the house..
The little boy told the lady that he wanted to live in her house. The lady told the little boy that he could live in her house if he could find a way to make her house even more beautiful..
The little boy thought about this for a while. He decided that he would make the lady’s house even more beautiful by making it into a castle..
The little boy went to work. He built a castle out of the house. He put a moat around the castle. He put a drawbridge over the moat. He put a drawbridge over the drawbridge. He put a drawbridge over the drawbridge..
The little boy was very happy with his castle...
```

## TODOs
- [x] Support chat interface.
- [x] Add tok/s.
- [ ] Support GPU inference.
- [ ] Support quantization.
- [ ] Support flash attention.
- [ ] Support AMD GPUs.
