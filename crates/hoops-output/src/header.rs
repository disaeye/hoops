use hoops_core::{
    validate_id_string, CanonicalLeBytes, CanonicalReader, DecodeError, MatchInitSpecV1,
    DEBUG_FRAME_BYTES, PUBLIC_FRAME_BYTES,
};
use serde::{Deserialize, Serialize};

pub const HPS1_MAGIC: [u8; 4] = *b"HPS1";
pub const HPS1_VERSION: u16 = 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum StreamVisibility {
    PublicOnly = 0,
    AuthoritativeFull = 1,
}

impl StreamVisibility {
    pub fn from_u8(v: u8) -> Result<Self, DecodeError> {
        match v {
            0 => Ok(Self::PublicOnly),
            1 => Ok(Self::AuthoritativeFull),
            _ => Err(DecodeError::InvalidEnumValue {
                field: "Hps1Header.stream_visibility",
                value: v as u32,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hps1RosterEntry {
    pub roster_idx: u8,
    pub jersey_number_code: u8,
    pub player_id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hps1Header {
    pub format_version: u16,
    pub stream_visibility: StreamVisibility,
    pub roster_count: u8,
    pub frame_size_bytes: u16,
    pub debug_frame_size_bytes: u16,
    pub court_length_mm: u32,
    pub court_width_mm: u32,
    pub home_team_id: String,
    pub away_team_id: String,
    pub roster: Vec<Hps1RosterEntry>,
    pub play_dict: Vec<(u16, String)>,
    pub def_dict: Vec<(u16, String)>,
}

impl Hps1Header {
    pub fn from_init_spec(spec: &MatchInitSpecV1, visibility: StreamVisibility) -> Self {
        let mut roster = Vec::with_capacity(30);
        for (rank, p) in spec.home_team.players.iter().enumerate().take(15) {
            roster.push(Hps1RosterEntry {
                roster_idx: rank as u8,
                jersey_number_code: p.jersey_number_code,
                player_id: p.player_id.clone(),
                display_name: p.display_name.clone(),
            });
        }
        // Pad home to 15 slots if fewer? Or keep packed <=30 with explicit roster_idx 0..15 / 15..30!
        // Use canonical fixed 30 entries (with roster_idx = 0..30) or actual players?
        // Using exact 30 entries keeps roster_idx == vec index directly! Wait: let's store actual active players sorted by roster_idx, and allow indexing by roster_idx.
        // Better: pad inactive slots with empty sentinel? No, validate_id_string forbids empty! Store actual active players (rank + team*15), and let header.roster store (0..Nhome, 15..15+Naway) OR store only active! Notice PublicFrameV1 has `roster_count: u8 = 30`, so player indices 15..27 are < 30!
        // Let's keep `roster_count = 30` when both teams fit 0..30, and for `roster` store present entries sorted by `roster_idx`.
        for (rank, p) in spec.away_team.players.iter().enumerate().take(15) {
            roster.push(Hps1RosterEntry {
                roster_idx: (15 + rank) as u8,
                jersey_number_code: p.jersey_number_code,
                player_id: p.player_id.clone(),
                display_name: p.display_name.clone(),
            });
        }

        let (play_dict, def_dict) = match visibility {
            StreamVisibility::PublicOnly => (Vec::new(), Vec::new()),
            StreamVisibility::AuthoritativeFull => (
                spec.playbooks
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (i as u16, p.play_id.as_str().to_owned()))
                    .collect(),
                spec.defenses
                    .iter()
                    .flat_map(|plan| &plan.pnr_rules)
                    .enumerate()
                    .map(|(i, rule)| (i as u16, rule.rule_id.as_str().to_owned()))
                    .collect(),
            ),
        };

        Self {
            format_version: HPS1_VERSION,
            stream_visibility: visibility,
            roster_count: 30,
            frame_size_bytes: PUBLIC_FRAME_BYTES as u16,
            debug_frame_size_bytes: if visibility == StreamVisibility::PublicOnly {
                0
            } else {
                DEBUG_FRAME_BYTES as u16
            },
            court_length_mm: (spec.ruleset.court.length_m * 1000.0).round() as u32,
            court_width_mm: (spec.ruleset.court.width_m * 1000.0).round() as u32,
            home_team_id: spec.home_team.team_id.clone(),
            away_team_id: spec.away_team.team_id.clone(),
            roster,
            play_dict,
            def_dict,
        }
    }

    pub fn lookup_player(&self, roster_idx: u8) -> Option<&Hps1RosterEntry> {
        self.roster.iter().find(|r| r.roster_idx == roster_idx)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        self.write_canonical_le(&mut out);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::from_canonical_le_bytes(bytes)
    }
}

impl CanonicalLeBytes for Hps1Header {
    fn write_canonical_le(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&HPS1_MAGIC);
        self.format_version.write_canonical_le(out);
        out.push(self.stream_visibility as u8);
        out.push(self.roster_count);
        self.frame_size_bytes.write_canonical_le(out);
        self.debug_frame_size_bytes.write_canonical_le(out);
        out.extend_from_slice(&[0u8; 4]); // reserved == 0
        self.court_length_mm.write_canonical_le(out);
        self.court_width_mm.write_canonical_le(out);
        self.home_team_id.write_canonical_le(out);
        self.away_team_id.write_canonical_le(out);
        (self.roster.len() as u8).write_canonical_le(out);
        for r in &self.roster {
            r.roster_idx.write_canonical_le(out);
            r.jersey_number_code.write_canonical_le(out);
            r.player_id.write_canonical_le(out);
            r.display_name.write_canonical_le(out);
        }
        self.play_dict.write_canonical_le(out);
        self.def_dict.write_canonical_le(out);
    }

    fn read_canonical_le(reader: &mut CanonicalReader<'_>) -> Result<Self, DecodeError> {
        let magic = reader.read_bytes(4)?;
        if magic != HPS1_MAGIC {
            return Err(DecodeError::InvalidMagic);
        }
        let format_version = u16::read_canonical_le(reader)?;
        if format_version != HPS1_VERSION {
            return Err(DecodeError::UnsupportedVersion {
                version: format_version,
            });
        }
        let stream_visibility = StreamVisibility::from_u8(reader.read_u8()?)?;
        let roster_count = reader.read_u8()?;
        if roster_count > 30 {
            return Err(DecodeError::InvalidFieldValue {
                field: "Hps1Header.roster_count",
            });
        }
        let frame_size_bytes = u16::read_canonical_le(reader)?;
        if frame_size_bytes as usize != PUBLIC_FRAME_BYTES {
            return Err(DecodeError::InvalidFieldValue {
                field: "Hps1Header.frame_size_bytes",
            });
        }
        let debug_frame_size_bytes = u16::read_canonical_le(reader)?;
        reader.read_zero_pad(4)?;
        let court_length_mm = u32::read_canonical_le(reader)?;
        let court_width_mm = u32::read_canonical_le(reader)?;
        let home_team_id = String::read_canonical_le(reader)?;
        validate_id_string(&home_team_id)?;
        let away_team_id = String::read_canonical_le(reader)?;
        validate_id_string(&away_team_id)?;

        let r_len = reader.read_u8()? as usize;
        if r_len > 30 {
            return Err(DecodeError::InvalidFieldValue {
                field: "Hps1Header.roster_len",
            });
        }
        let mut roster = Vec::with_capacity(r_len);
        for _ in 0..r_len {
            let roster_idx = reader.read_u8()?;
            if roster_idx >= roster_count {
                return Err(DecodeError::InvalidRosterIndex {
                    idx: roster_idx,
                    roster_count,
                });
            }
            let jersey_number_code = reader.read_u8()?;
            if jersey_number_code > 100 {
                return Err(DecodeError::InvalidEnumValue {
                    field: "jersey_number_code",
                    value: jersey_number_code as u32,
                });
            }
            let player_id = String::read_canonical_le(reader)?;
            validate_id_string(&player_id)?;
            let display_name = String::read_canonical_le(reader)?;
            validate_id_string(&display_name)?;
            roster.push(Hps1RosterEntry {
                roster_idx,
                jersey_number_code,
                player_id,
                display_name,
            });
        }
        let play_dict = Vec::<(u16, String)>::read_canonical_le(reader)?;
        let def_dict = Vec::<(u16, String)>::read_canonical_le(reader)?;
        if stream_visibility == StreamVisibility::PublicOnly
            && (!play_dict.is_empty() || !def_dict.is_empty())
        {
            return Err(DecodeError::PrivateDictInPublicStream);
        }

        Ok(Self {
            format_version,
            stream_visibility,
            roster_count,
            frame_size_bytes,
            debug_frame_size_bytes,
            court_length_mm,
            court_width_mm,
            home_team_id,
            away_team_id,
            roster,
            play_dict,
            def_dict,
        })
    }
}
