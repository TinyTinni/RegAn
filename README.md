# RegAn

![banner](/simulation/eval/site_preview.jpg)

[![Rust](https://github.com/TinyTinni/RegAn/actions/workflows/rust.yml/badge.svg)](https://github.com/TinyTinni/RegAn/actions/workflows/rust.yml)

A tool to create an annotated set of strictly ordered data based on hard to rate images. By doing relational comparisons for each image pair, it is possible to get an ordering with lower bias. 
It can be used to rate the quality of natural products like the cookies, which cannot fully objectively described.
The tool automatically selects the next lowest rated image and finds a suitable candidate in order to lower the amount comparisons for the highest ordering certainty (see simulation for details). The rating, uncertainty and comparisons are saved in a SQLite database. 

The frontend is a HTML site which works on desktop and mobile. The server is a simple standalone binary, so it can run almost everywhere. You can host it or run it locally.


## Motivation
At one of my jobs in the computer vision field, I had multiple customers who tried to assure the quality of their products using camera systems. Due to some huge variances of some products, mostly nature products like e.g. sugar beets, was the correct measurement metric. While experts could assess those products, the metric was not well defined and the experts eyeballed it. Also, we could see a clear bias on the quality assessment, depending on the previous batch of products the expert saw. For example, if the expert saw a series of really bad products, then a product which he usually would put in midrange, the expert would most likely overestimate the quality of the not-bad product.

This program is for annotating a ranking of quality of given product images and given a not well-defined metric.

## What is inside ?
There are 2 binaries, the server and a simulation.

### Server
The server provides a platform for comparing images. It will select images from a given file directory and present "games" (aka a pair of pictures) via a web interface. The server also computes after each game the new elo rank of the involved images and will select a new match based on the rankings.

The matchmaking strategy of the server tries to select those images with the highest uncertainty about their rank.

#### Parameters
Use `--help` to see the parameter documentation

Default, the listens to port 8000, but it can be changed with the `--port` parameter.

The server will use images provided in a `images` subdirectory. Later on, you can add or remove images, but to take an effect, the server requires a restart. The subdirectory can be changed with the `image_dir` parameter.

All games and the current ranking of each image is saved in an SQLite database. Default is currently the `out.db` file,
but can be changed with `-o` parameter. Using SQLite allows the server to run locally when the images are present, e.g. you can run the whole server from an USB stick.


### Simulator
One recurring question is, how many games are required to get to an acceptable level of error in the ranking. This can be calculated using the simulator. The simulator generates N many samples with strict and known order (the numbers 1..N). As the we know the order already, the simulator can play games and always choose a winner (a<b => b is the winner).

One example is shown here:  
![original](/simulation/eval/simulation_original.gif)

Here, each image has a rank at the beginning (1-500), their are initially randomly ordered. You can say, that after 4000 games, the MSRE is at around 25. You can decide now, if the precision is good enough or if it is too much and you
can also perform other tasks with less games. The yellow bar indicates the uncertainty of the ranking.

Furthermore, you can linearize those ratings and rank all the images. You just order them from lowest to highest ranting and assume that the difference between each rating is equal. Therefor, you would get something that looks like this:
![linear](/simulation/eval/simulation_linear.gif)

### Simulator options and further information

The simulator can be configured with how many samples should be used and how many games should be played. The resulting rankings of our known ordered numbers and the error of the ranking can be computed.

Futhermore, it is possible to simulate some kind of uncertainty in the ranking process. As this task's problem is inherently difficult with clear ranking of objects, it is likely that even a human expert cannot clearly and unambiguous differentiate samples with similar ranking and give misleading answers. For example, they can differentiate a 0 and a 100, but not clearly differentiate a 30 and a 50 (as a picture) as they are too similar. This uncertainty can be modeled in the simulation giving `std_dev`. It will randomly add/sub a normal distributed random number to the ranked numbers.

The simulation also helps to review different matchmaking algorithms. The goal for matchmaking algorithms is to lower numbers of required games for a specific error.

# License
[AGPL License](./LICENSE) © Matthias Möller. Made with ❤ in Germany.
