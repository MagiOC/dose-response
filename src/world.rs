use blocker::Blocker;
use formula;
use generators::{self, GeneratedWorld};
use item::{self, Item};
use level::{self, Cell, Level};
use monster::Monster;
use player::{self, PlayerInfo};
use point::{CircularArea, Point, SquareArea};

use rand::{IsaacRng, Rng};
use ranged_int::InclusiveRange;
use rect::Rectangle;
use std::collections::HashMap;

#[derive(Serialize, Deserialize)]
pub struct Chunk {
    position: Point,
    pub rng: IsaacRng,
    pub level: Level,
    monsters: Vec<Monster>,
}

impl Chunk {
    fn new(world_seed: u32, position: ChunkPosition, size: i32, player_position: Point) -> Self {
        use std::num::Wrapping;
        let pos = position.position;
        // NOTE: `x` and `y` overflow on negative values here, but all
        // we care about is having a distinct value for each position
        // so our seeds don't repeat. So this is fine here.
        let seed = Wrapping(world_seed as u64) + (Wrapping(13) * Wrapping(pos.x as u64))
            + (Wrapping(17) * Wrapping(pos.y as u64));

        // TODO: Monsters in different chunks will now have identical
        // IDs. We need to investigate whether that's a problem.

        let mut chunk = Chunk {
            position: pos,
            rng: IsaacRng::new_from_u64(seed.0),
            level: Level::new(size, size),
            monsters: vec![],
        };

        let mut throwavay_rng = chunk.rng.clone();
        let generated_data = generators::forrest::generate(
            &mut chunk.rng,
            &mut throwavay_rng,
            chunk.level.size(),
            player_position,
        );

        chunk.populate(generated_data);

        chunk
    }

    fn populate(&mut self, generated_world: GeneratedWorld) {
        let (map, generated_monsters, items) = generated_world;
        for &(pos, item) in map.iter() {
            let pos = self.level.level_position(pos);
            self.level.set_tile(pos, item);
        }
        for (index, mut monster) in generated_monsters.into_iter().enumerate() {
            // TODO: the pos conversion would not be necessary if the
            // worldgen operated with world positions in the first
            // place.
            let pos = self.level.level_position(monster.position);
            assert!(self.level.walkable(pos, Blocker::WALL | Blocker::MONSTER));
            monster.position = self.world_position(pos);
            self.monsters.push(monster);
            self.level.set_monster(pos, index);
            assert!(!self.level.walkable(pos, Blocker::WALL | Blocker::MONSTER));
        }
        for &(pos, item) in items.iter() {
            let pos = self.level.level_position(pos);
            assert!(self.level.walkable(pos, Blocker::WALL));
            self.level.add_item(pos, item);
        }
    }

    pub fn level_position(&self, world_position: Point) -> level::LevelPosition {
        self.level.level_position(world_position - self.position)
    }

    pub fn world_position(&self, level_position: level::LevelPosition) -> Point {
        let level_position: Point = level_position.into();
        self.position + level_position
    }

    pub fn cells(&self) -> ChunkCells {
        ChunkCells {
            chunk_position: self.position,
            cells: self.level.iter(),
        }
    }

    pub fn monsters(&self) -> ::std::slice::Iter<Monster> {
        self.monsters.iter()
    }

    pub fn monsters_mut<'a>(&'a mut self) -> impl Iterator<Item = &'a mut Monster> {
        self.monsters.iter_mut()
    }
}

pub struct ChunkCells<'a> {
    chunk_position: Point,
    cells: level::Cells<'a>,
}

impl<'a> Iterator for ChunkCells<'a> {
    type Item = (Point, &'a Cell);

    fn next(&mut self) -> Option<Self::Item> {
        self.cells.next().map(|(level_pos, cell)| {
            let offset: Point = level_pos.into();
            (self.chunk_position + offset, cell)
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct ChunkPosition {
    position: Point,
}

#[derive(Serialize, Deserialize)]
pub struct World {
    seed: u32,
    max_half_size: i32,
    chunk_size: i32,
    chunks: HashMap<ChunkPosition, Chunk>,
}

impl World {
    pub fn new<R: Rng>(
        rng: &mut R,
        seed: u32,
        dimension: i32,
        chunk_size: i32,
        player_info: PlayerInfo,
    ) -> Self {
        assert!(dimension > 0);
        assert!(chunk_size > 0);
        assert_eq!(dimension % 2, 0);
        assert_eq!(dimension % chunk_size, 0);

        let mut world = World {
            seed,
            max_half_size: dimension / 2,
            chunk_size,
            chunks: HashMap::new(),
        };

        // TODO: I don't think this code belongs in World. Move it
        // into the level generators or osmething?
        world.prepare_initial_playing_area(player_info, rng);
        world
    }

    /// Remove some of the monsters from player's initial vicinity,
    /// place some food nearby and a dose in sight.
    fn prepare_initial_playing_area<R: Rng>(&mut self, player_info: PlayerInfo, rng: &mut R) {
        assert!(formula::INITIAL_SAFE_RADIUS <= formula::INITIAL_EASY_RADIUS);

        let safe_area = Rectangle::center(
            player_info.pos,
            Point::from_i32(formula::INITIAL_SAFE_RADIUS),
        );

        let easy_area = Rectangle::center(
            player_info.pos,
            Point::from_i32(formula::INITIAL_EASY_RADIUS),
        );

        for pos in easy_area.points() {
            self.ensure_chunk_at_pos(pos);
        }

        // Remove monsters from the starting area
        for pos in easy_area.points() {
            let remove_monster = self.monster_on_pos(pos).map_or(false, |m| {
                use monster::Kind::*;
                let easy_monster = match m.kind {
                    Shadows | Voices => false,
                    Hunger | Anxiety | Depression | Npc => true,
                };
                safe_area.contains(pos) || easy_monster
            });
            if remove_monster {
                self.remove_monster(pos)
            }
        }

        // Remove strong doses from the starting area
        let no_lethal_dose_area = Rectangle::center(
            player_info.pos,
            Point::from_i32(formula::NO_LETHAL_DOSE_RADIUS),
        );

        // Clear any doses whos irresistible area touches the player's
        // position.
        {
            let resist_radius =
                formula::player_resist_radius(formula::DOSE_PREFAB.irresistible, player_info.will);
            let resist_area = Rectangle::center(player_info.pos, Point::from_i32(resist_radius));
            for point in resist_area.points() {
                if let Some(cell) = self.cell_mut(point) {
                    for index in (0..cell.items.len()).rev() {
                        if cell.items[index].is_dose() {
                            cell.items.remove(index);
                        }
                    }
                }
            }
        }

        for pos in no_lethal_dose_area.points() {
            if let Some(cell) = self.cell_mut(pos) {
                for index in (0..cell.items.len()).rev() {
                    use item::Kind::*;
                    let lethal_dose = match cell.items[index].kind {
                        Food | Dose => false,
                        StrongDose | CardinalDose | DiagonalDose => true,
                    };
                    if lethal_dose {
                        cell.items.remove(index);
                    }
                }
            }
        }

        // Generate a usable dose nearby, give up after 50 attempts
        for _ in 0..50 {
            let offset = Point {
                x: rng.gen_range(-3, 4),
                y: rng.gen_range(-3, 4),
            };
            if offset == (0, 0) {
                continue;
            }
            let pos = player_info.pos + offset;
            if self.walkable(pos, Blocker::WALL, player_info.pos) {
                // Skip if there already is an item at the position
                if !self.cell(pos).map_or(true, |cell| cell.items.is_empty()) {
                    continue;
                }

                let dose = Item {
                    kind: item::Kind::Dose,
                    modifier: player::Modifier::Intoxication {
                        state_of_mind: 70,
                        tolerance_increase: 1,
                    },
                    irresistible: 2,
                };

                let resist_radius =
                    formula::player_resist_radius(dose.irresistible, player_info.will);
                let resist_area = Rectangle::center(pos, Point::from_i32(resist_radius));

                // Bail if the player would be in the resist radius
                if resist_area.contains(player_info.pos) {
                    continue;
                }

                // Bail if another dose is in the resist area
                if resist_area.points().any(|irresistable_point| {
                    self.cell(irresistable_point)
                        .map_or(false, |cell| cell.items.iter().any(|item| item.is_dose()))
                }) {
                    continue;
                }

                // Try to place the dose and exit
                if let Some(chunk) = self.chunk_mut(pos) {
                    let level_position = chunk.level_position(pos);
                    chunk.level.add_item(level_position, dose);
                    break;
                }
            }
        }

        // Generate food near the starting area, bail after 50 attempts
        let mut amount_of_food_to_generate = rng.gen_range(1, 4);
        for _ in 0..50 {
            let offset = Point {
                x: rng.gen_range(-5, 6),
                y: rng.gen_range(-5, 6),
            };
            let pos = player_info.pos + offset;
            if self.walkable(pos, Blocker::WALL, player_info.pos) {
                let food = Item {
                    kind: item::Kind::Food,
                    modifier: player::Modifier::Attribute {
                        state_of_mind: 10,
                        will: 0,
                    },
                    irresistible: 0,
                };
                if let Some(chunk) = self.chunk_mut(pos) {
                    let level_position = chunk.level_position(pos);
                    if chunk.level.cell(level_position).items.is_empty() {
                        chunk.level.add_item(level_position, food);
                        amount_of_food_to_generate -= 1;
                    }
                }

                if amount_of_food_to_generate <= 0 {
                    break;
                }
            }
        }

        // Remove anything at the player's position
        if let Some(cell) = self.cell_mut(player_info.pos) {
            cell.items.clear();
        }
    }

    /// Return the ChunkPosition for a given point within the chunk.
    ///
    /// Chunks have equal width and height and can have negative
    /// positions. There is a chunk at `(0, 0)` and then at
    /// `(-chunk_size, 0)`, `(chunk_size, 0)` and so on.
    fn chunk_pos_from_world_pos(&self, pos: Point) -> ChunkPosition {
        let chunk_pos = |num: i32| {
            let size = self.chunk_size;
            if num >= 0 {
                (num / size) * size
            } else {
                (-(((-num - 1) / size) + 1)) * size
            }
        };

        ChunkPosition {
            position: Point {
                x: chunk_pos(pos.x),
                y: chunk_pos(pos.y),
            },
        }
    }

    /// Get the chunk at the given world position. This means it
    /// doesn't have to match chunk's internal position -- any point
    /// within that Chunk will do.
    pub fn chunk(&self, pos: Point) -> Option<&Chunk> {
        let chunk_position = self.chunk_pos_from_world_pos(pos);
        self.chunks.get(&chunk_position)
    }

    /// Get the mutable chunk at the given world position. This means
    /// it doesn't have to match chunk's internal position -- any
    /// point within that Chunk will do.
    pub fn chunk_mut(&mut self, pos: Point) -> Option<&mut Chunk> {
        let chunk_position = self.chunk_pos_from_world_pos(pos);
        self.chunks.get_mut(&chunk_position)
    }

    pub fn ensure_chunk_at_pos(&mut self, pos: Point) {
        let chunk_position = self.chunk_pos_from_world_pos(pos);

        let seed = self.seed;
        let chunk_size = self.chunk_size;
        // TODO: figure out how to generate the starting chunks so the
        // player has some doses and food and no monsters.
        self.chunks
            .entry(chunk_position)
            .or_insert_with(|| Chunk::new(seed, chunk_position, chunk_size, (0, 0).into()));
    }

    fn cell(&self, world_pos: Point) -> Option<&Cell> {
        let chunk = self.chunk(world_pos);
        // NOTE: the positions within a chunk/level start from zero so
        // we need to de-offset them with the chunk position.
        chunk.map(|chunk| {
            let level_position = chunk.level_position(world_pos);
            chunk.level.cell(level_position)
        })
    }

    pub fn cell_mut(&mut self, world_pos: Point) -> Option<&mut Cell> {
        let chunk = self.chunk_mut(world_pos);
        // NOTE: the positions within a chunk/level start from zero so
        // we need to de-offset them with the chunk position.
        chunk.map(|chunk| {
            let level_position = chunk.level_position(world_pos);
            chunk.level.cell_mut(level_position)
        })
    }

    /// Check whether the given position is within the bounds of the World.
    ///
    /// While the world should be "technically infinite", we well have
    /// some sort of upper limit on the positions it's able to
    /// support.
    pub fn within_bounds(&self, pos: Point) -> bool {
        pos.x < self.max_half_size && pos.x > -self.max_half_size && pos.y < self.max_half_size
            && pos.y > -self.max_half_size
    }

    /// Check whether the given position is walkable.
    ///
    /// Points outside of the World are not walkable. The
    /// `blockers` option controls can influence the logic: are
    /// monster treated as blocking or not?
    pub fn walkable(&self, pos: Point, blockers: Blocker, player_pos: Point) -> bool {
        let level_cell_walkable = self.chunk(pos)
            .map(|chunk| {
                let blocks_player = blockers.contains(Blocker::PLAYER) && pos == player_pos;
                let level_position = chunk.level_position(pos);
                chunk
                    .level
                    .walkable(level_position, blockers - Blocker::PLAYER)
                    && !blocks_player
            })
            .unwrap_or(false);
        self.within_bounds(pos) && level_cell_walkable
    }

    /// Pick up the top `Item` stacked on the tile. If the position is
    /// not withing bounds, nothing happens.
    pub fn pickup_item(&mut self, pos: Point) -> Option<Item> {
        if self.within_bounds(pos) {
            self.cell_mut(pos).and_then(|cell| cell.items.pop())
        } else {
            None
        }
    }

    /// If there's a monster at the given tile, return its ID.
    ///
    /// Returns `None` if there is no monster or if `pos` is out of bounds.
    pub fn monster_on_pos(&mut self, world_pos: Point) -> Option<&mut Monster> {
        if self.within_bounds(world_pos) {
            if let Some(chunk) = self.chunk_mut(world_pos) {
                let level_position = chunk.level_position(world_pos);
                chunk
                    .level
                    .monster_on_pos(level_position)
                    .and_then(move |monster_index| Some(&mut chunk.monsters[monster_index]))
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Move the monster from one place in the world to the destination.
    /// If the paths are identical, nothing happens.
    /// Panics if the destination is out of bounds or already occupied.
    pub fn move_monster(
        &mut self,
        monster_position: Point,
        destination: Point,
        player_position: Point,
    ) {
        if monster_position == destination {
            return;
        }
        let blocker = Blocker::PLAYER | Blocker::MONSTER | Blocker::WALL;
        assert!(
            self.walkable(destination, blocker, player_position),
            "Moster at {:?} cannot move to {:?} because it's occupied.",
            monster_position,
            destination
        );
        let monster_chunk_pos = self.chunk_pos_from_world_pos(monster_position);
        let destination_chunk_pos = self.chunk_pos_from_world_pos(destination);
        if monster_chunk_pos == destination_chunk_pos {
            if let Some(monster) = self.monster_on_pos(monster_position) {
                monster.position = destination;
            }
            let chunk = self.chunk_mut(monster_position).expect(&format!(
                "Chunk with monster {:?} doesn't exist.",
                monster_position
            ));
            let level_monster_pos = chunk.level_position(monster_position);
            let level_destination_pos = chunk.level_position(destination);
            chunk
                .level
                .move_monster(level_monster_pos, level_destination_pos);
        } else {
            // Need to move the monster to another chunk
            // NOTE: We're not removing the monster from the
            // `chunk.monsters` vec in order not to mess up with the
            // indices there.
            //
            // Instead, we make it dead here (without any of the
            // normal connotations) and just remove it from the level.
            let mut new_monster = {
                let monster = self.monster_on_pos(monster_position).expect(
                    "Trying to move a monster, but there's nothing \
                     there.",
                );
                let result = monster.clone();
                monster.dead = true;
                result
            };

            {
                self.remove_monster(monster_position);
                assert!(self.walkable(monster_position, blocker, player_position));
                new_monster.position = destination;
                let destination_chunk = self.chunk_mut(destination).expect(&format!(
                    "Destination chunk at {:?} doesn't \
                     exist.",
                    destination
                ));
                let new_monster_index = destination_chunk.monsters.len();
                destination_chunk.monsters.push(new_monster);
                let destination_level_position = destination_chunk.level_position(destination);
                destination_chunk
                    .level
                    .set_monster(destination_level_position, new_monster_index);
            }

            assert!(!self.walkable(destination, Blocker::MONSTER, player_position));
        }
    }

    /// Remove the monster at the given position (if there is any
    /// there) from the world.
    pub fn remove_monster(&mut self, pos: Point) {
        if let Some(chunk) = self.chunk_mut(pos) {
            let level_position = chunk.level_position(pos);
            let index = chunk.level.monsters.remove(&level_position);
            // TODO: we should figure out a better way of removing
            // monsters from the map.
            if let Some(index) = index {
                chunk.monsters[index].dead = true;
            }
        }
    }

    /// Set cells within the given radius as explored.
    pub fn explore(&mut self, centre: Point, radius: i32) {
        for pos in CircularArea::new(centre, radius) {
            if self.within_bounds(pos) {
                if let Some(cell) = self.cell_mut(pos) {
                    cell.explored = true;
                }
            }
        }
    }

    /// Get a dose within the given radius that's nearest to the
    /// specified point.
    pub fn nearest_dose(&mut self, centre: Point, radius: i32) -> Option<(Point, Item)> {
        let mut doses = vec![];
        for pos in CircularArea::new(centre, radius) {
            // Make sure we don't go out of bounds with self.cell(pos):
            // NOTE: We're not checking for the player's position here so we'll just supply a
            // dummy value.
            if !self.walkable(pos, Blocker::WALL, Point::new(0, 0)) {
                continue;
            }
            doses.extend(
                self.cell(pos)
                    .map_or(vec![].iter(), |cell| cell.items.iter())
                    .filter(|i| i.is_dose())
                    .map(|&item| (pos, item)),
            );
        }

        doses.pop().map(|dose| {
            let mut result = dose;
            for d in &doses {
                if centre.tile_distance(d.0) < centre.tile_distance(result.0) {
                    result = *d;
                }
            }
            result
        })
    }

    /// Return a random walkable position next to the given point.
    ///
    /// If there is no such position available, return `starting_pos`.
    pub fn random_neighbour_position<T: Rng>(
        &self,
        rng: &mut T,
        starting_pos: Point,
        blockers: Blocker,
        player_position: Point,
    ) -> Point {
        let mut walkables = vec![];
        for pos in SquareArea::new(starting_pos, 2) {
            if pos != starting_pos && self.walkable(pos, blockers, player_position) {
                walkables.push(pos)
            }
        }
        match rng.choose(&walkables) {
            Some(&random_pos) => random_pos,
            None => starting_pos, // Nowhere to go
        }
    }

    pub fn random_position_in_range<T: Rng>(
        &self,
        rng: &mut T,
        starting_position: Point,
        range: InclusiveRange,
        max_tries: u32,
        blockers: Blocker,
        player_position: Point,
    ) -> Option<Point> {
        for _ in 0..max_tries {
            let offset = Point::new(
                rng.gen_range(-range.1, range.1 + 1),
                rng.gen_range(-range.1, range.1 + 1),
            );
            let candidate = starting_position + offset;
            if offset.x.abs() > range.0 && offset.y.abs() > range.0
                && self.walkable(candidate, blockers, player_position)
            {
                return Some(candidate);
            }
        }
        None
    }

    /// Returns an iterator over chunks that intersect with the given
    /// area.
    ///
    /// NOTE: The order of the chunks is not specified.
    pub fn chunks<'a>(&'a self, area: Rectangle) -> impl Iterator<Item = &'a Chunk> {
        let chunk_size = self.chunk_size;
        self.chunks
            .iter()
            .filter(move |&(pos, ref _chunk)| {
                let chunk_area =
                    Rectangle::from_point_and_size(pos.position, Point::from_i32(chunk_size));
                area.intersects(chunk_area)
            })
            .map(move |(_pos, chunk)| chunk)
    }

    /// Returns a mutable iterator over chunks that intersect with the
    /// given area.
    ///
    /// NOTE: The order of the chunks is not specified.
    pub fn chunks_mut<'a>(&'a mut self, area: Rectangle) -> impl Iterator<Item = &'a mut Chunk> {
        let chunk_size = self.chunk_size;
        self.chunks
            .iter_mut()
            .filter(move |&(pos, ref _chunk)| {
                let chunk_area =
                    Rectangle::from_point_and_size(pos.position, Point::from_i32(chunk_size));
                area.intersects(chunk_area)
            })
            .map(move |(_pos, chunk)| chunk)
    }

    /// Return an iterator over all monsters in the given area.
    ///
    /// NOTE: The order of the monsters is not specified.
    pub fn monsters<'a>(&'a self, area: Rectangle) -> impl Iterator<Item = &'a Monster> {
        self.chunks(area)
            .flat_map(Chunk::monsters)
            .filter(move |m| m.alive() && area.contains(m.position))
    }

    /// Return a mutable iterator over all monsters in the given area.
    ///
    /// NOTE: The order of the monsters is not specified.
    pub fn monsters_mut<'a>(
        &'a mut self,
        area: Rectangle,
    ) -> impl Iterator<Item = &'a mut Monster> {
        self.chunks_mut(area)
            .flat_map(Chunk::monsters_mut)
            .filter(move |m| m.alive() && area.contains(m.position))
    }

    pub fn positions_of_all_chunks(&self) -> Vec<Point> {
        self.chunks
            .keys()
            .map(|chunk_pos| chunk_pos.position)
            .collect()
    }
}
