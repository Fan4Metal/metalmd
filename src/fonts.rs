//! Fonts the reader picks in the Text panel. The system's families are
//! listed with fontdb on a background thread when the panel asks for them,
//! never at startup; a chosen family is resolved there to the files (and,
//! for a variable font, the axis values) of its regular, bold, italic and
//! bold italic faces, which are kept in the settings. Startup then reads
//! those files as it reads the default fonts, at the same cost.

use egui::FontData;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

/// One face of a font file: the file, the face's number in a collection
/// (`.ttc`), and values for the axes of a variable font (`wght`, `ital`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceRef {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub index: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coords: Vec<(String, f32)>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A family the reader chose, its faces resolved. A style the family lacks
/// is `None`: bold is then drawn regular, and italic slanted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FontSpec {
    pub family: String,
    pub regular: FaceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<FaceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<FaceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold_italic: Option<FaceRef>,
}

/// A face of a system family, as fontdb found it.
#[derive(Clone, Debug)]
pub struct Face {
    pub path: PathBuf,
    pub index: u32,
    pub weight: u16,
    pub italic: bool,
}

/// A system font family.
#[derive(Clone, Debug)]
pub struct Family {
    pub name: String,
    /// One of its faces says it is monospaced.
    pub monospaced: bool,
    pub faces: Vec<Face>,
}

/// The installed font families, by name. Reads the header of every font
/// file of the system and of the user (tens of milliseconds, more with a
/// cold disk): for a background thread.
pub fn system_families() -> Vec<Family> {
    use fontdb::{Language, Source, Style};
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut families: BTreeMap<String, Family> = BTreeMap::new();
    for face in db.faces() {
        let path = match &face.source {
            Source::File(path) | Source::SharedFile(path, _) => path.clone(),
            Source::Binary(_) => continue,
        };
        let name = face
            .families
            .iter()
            .find(|(_, language)| *language == Language::English_UnitedStates)
            .or_else(|| face.families.first())
            .map(|(name, _)| name.clone());
        // `@` names are the vertical forms of East Asian fonts.
        let Some(name) = name.filter(|n| !n.is_empty() && !n.starts_with('@')) else {
            continue;
        };
        let family = families
            .entry(name.to_lowercase())
            .or_insert_with(|| Family {
                name,
                monospaced: false,
                faces: Vec::new(),
            });
        family.monospaced |= face.monospaced;
        family.faces.push(Face {
            path,
            index: face.index,
            weight: face.weight.0,
            italic: face.style != Style::Normal,
        });
    }
    families.into_values().collect()
}

/// A variation axis: its tag (`wght`), least and greatest value.
#[derive(Clone, Debug, PartialEq)]
pub struct Axis {
    pub tag: String,
    pub min: f32,
    pub max: f32,
}

/// The variation axes of a face; empty for a static font or one that
/// cannot be read.
fn axes_of(path: &Path, index: u32) -> Vec<Axis> {
    use skrifa::MetadataProvider as _;
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let Ok(font) = skrifa::FontRef::from_index(&bytes, index) else {
        return Vec::new();
    };
    font.axes()
        .iter()
        .map(|axis| Axis {
            tag: axis.tag().to_string(),
            min: axis.min_value(),
            max: axis.max_value(),
        })
        .collect()
}

/// The faces of `family` for each style. Reads the files whose axes it
/// needs: for a background thread.
pub fn resolve(family: &Family) -> Option<FontSpec> {
    let mut cache = HashMap::new();
    let mut axes = |path: &Path, index: u32| -> Vec<Axis> {
        cache
            .entry((path.to_path_buf(), index))
            .or_insert_with(|| axes_of(path, index))
            .clone()
    };
    resolve_with(family, &mut axes)
}

/// [`resolve`], with the axes of a face given by `axes`.
fn resolve_with(
    family: &Family,
    axes: &mut dyn FnMut(&Path, u32) -> Vec<Axis>,
) -> Option<FontSpec> {
    // The face of the style nearest `weight`, among those `fits` allows.
    let nearest = |italic: bool, weight: u16, fits: &dyn Fn(u16) -> bool| {
        family
            .faces
            .iter()
            .filter(|f| f.italic == italic && fits(f.weight))
            .min_by_key(|f| f.weight.abs_diff(weight))
    };
    let to_ref = |face: &Face| FaceRef {
        path: face.path.clone(),
        index: face.index,
        coords: Vec::new(),
    };
    // A face with one more axis value, if its file has that axis; the value
    // is kept within the axis.
    let mut with_axis = |base: &FaceRef, tag: &str, value: f32| -> Option<FaceRef> {
        let axis = axes(&base.path, base.index)
            .into_iter()
            .find(|a| a.tag == tag)?;
        let mut face = base.clone();
        face.coords.retain(|(t, _)| t != tag);
        face.coords
            .push((tag.to_owned(), value.clamp(axis.min, axis.max)));
        Some(face)
    };
    let any = |_: u16| true;
    let bold_weight = |w: u16| w >= 600;
    let book_weight = |w: u16| w < 600;

    let regular = nearest(false, 400, &any)
        .or_else(|| nearest(true, 400, &any))
        .map(to_ref)?;
    let bold = match nearest(false, 700, &bold_weight) {
        Some(face) => Some(to_ref(face)),
        None => with_axis(&regular, "wght", 700.0),
    };
    // An italic axis (`ital` 1) or a slant one (`slnt`, negative leans
    // right) stands in for an italic face.
    fn slanted(
        with_axis: &mut dyn FnMut(&FaceRef, &str, f32) -> Option<FaceRef>,
        base: &FaceRef,
    ) -> Option<FaceRef> {
        with_axis(base, "ital", 1.0).or_else(|| with_axis(base, "slnt", -12.0))
    }
    let italic = match nearest(true, 400, &book_weight) {
        Some(face) => Some(to_ref(face)),
        None => slanted(&mut with_axis, &regular),
    };
    let bold_italic = match nearest(true, 700, &bold_weight) {
        Some(face) => Some(to_ref(face)),
        None => match italic
            .as_ref()
            .and_then(|italic| with_axis(italic, "wght", 700.0))
        {
            Some(face) => Some(face),
            None => bold.as_ref().and_then(|bold| slanted(&mut with_axis, bold)),
        },
    };
    Some(FontSpec {
        family: family.name.clone(),
        regular,
        bold,
        italic,
        bold_italic,
    })
}

/// Reads a face for egui, or `None` if its file is gone or egui could not
/// parse it (egui panics on a font it cannot parse, and the release build
/// aborts on a panic).
pub fn read(face: &FaceRef) -> Option<FontData> {
    let bytes = std::fs::read(&face.path).ok()?;
    skrifa::FontRef::from_index(&bytes, face.index).ok()?;
    let mut data = FontData::from_owned(bytes);
    data.index = face.index;
    data.tweak.coords = egui::epaint::text::VariationCoords::new(
        face.coords
            .iter()
            .map(|(tag, value)| (tag.as_str(), *value)),
    );
    Some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(file: &str, weight: u16, italic: bool) -> Face {
        Face {
            path: PathBuf::from(file),
            index: 0,
            weight,
            italic,
        }
    }

    fn file(spec: &Option<FaceRef>) -> Option<&str> {
        spec.as_ref().and_then(|f| f.path.to_str())
    }

    #[test]
    fn static_faces_are_matched_by_weight_and_style() {
        let family = Family {
            name: "Segoe UI".into(),
            monospaced: false,
            faces: vec![
                face("light.ttf", 300, false),
                face("regular.ttf", 400, false),
                face("semibold.ttf", 600, false),
                face("bold.ttf", 700, false),
                face("italic.ttf", 400, true),
                face("bolditalic.ttf", 700, true),
            ],
        };
        let spec = resolve_with(&family, &mut |_, _| Vec::new()).unwrap();
        assert_eq!(spec.regular.path, PathBuf::from("regular.ttf"));
        assert_eq!(file(&spec.bold), Some("bold.ttf"));
        assert_eq!(file(&spec.italic), Some("italic.ttf"));
        assert_eq!(file(&spec.bold_italic), Some("bolditalic.ttf"));
    }

    #[test]
    fn a_family_without_bold_or_italic_leaves_them_out() {
        let family = Family {
            name: "Plain".into(),
            monospaced: true,
            faces: vec![face("plain.ttf", 400, false)],
        };
        let spec = resolve_with(&family, &mut |_, _| Vec::new()).unwrap();
        assert_eq!(spec.bold, None);
        assert_eq!(spec.italic, None);
        assert_eq!(spec.bold_italic, None);
    }

    #[test]
    fn a_variable_font_gets_its_styles_from_its_axes() {
        let family = Family {
            name: "Variable".into(),
            monospaced: false,
            faces: vec![face("var.ttf", 400, false)],
        };
        let axes = |_: &Path, _: u32| {
            vec![
                Axis {
                    tag: "wght".into(),
                    min: 200.0,
                    max: 650.0,
                },
                Axis {
                    tag: "slnt".into(),
                    min: -10.0,
                    max: 0.0,
                },
            ]
        };
        let spec = resolve_with(&family, &mut { axes }).unwrap();
        let coords = |f: &Option<FaceRef>| f.as_ref().unwrap().coords.clone();
        // Kept within the axis.
        assert_eq!(coords(&spec.bold), [("wght".to_owned(), 650.0)]);
        assert_eq!(coords(&spec.italic), [("slnt".to_owned(), -10.0)]);
        assert_eq!(
            coords(&spec.bold_italic),
            [("slnt".to_owned(), -10.0), ("wght".to_owned(), 650.0)]
        );
        assert_eq!(spec.regular.coords, []);
    }

    #[test]
    fn specs_survive_the_settings_file() {
        let spec = FontSpec {
            family: "Var".into(),
            regular: FaceRef {
                path: PathBuf::from(r"C:\Windows\Fonts\var.ttf"),
                index: 0,
                coords: Vec::new(),
            },
            bold: Some(FaceRef {
                path: PathBuf::from(r"C:\Windows\Fonts\var.ttf"),
                index: 1,
                coords: vec![("wght".into(), 700.0)],
            }),
            italic: None,
            bold_italic: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(!json.contains("italic"));
        assert_eq!(serde_json::from_str::<FontSpec>(&json).unwrap(), spec);
    }
}
