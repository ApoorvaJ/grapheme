use crate::*;

pub struct Pass<'a> {
    pub _name: String,
    pub outputs_color: Vec<&'a Texture>, // The textures must live at least as long as the pass
    pub opt_output_depth: Option<&'a Texture>,
    pub lambda: Box<dyn FnMut(vk::CommandBuffer)>, // TODO: Investigate async/await
}

impl<'a> Pass<'a> {
    pub fn new(name: &str, lambda: impl FnMut(vk::CommandBuffer) + 'static) -> Pass {
        Pass {
            _name: String::from(name),
            outputs_color: Vec::new(),
            opt_output_depth: None,
            lambda: Box::new(lambda),
        }
    }

    pub fn with_output_color(mut self, texture: &'a Texture) -> Pass {
        self.outputs_color.push(texture);
        self
    }

    pub fn with_output_depth(mut self, texture: &'a Texture) -> Pass {
        self.opt_output_depth = Some(texture);
        self
    }
}
