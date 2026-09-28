use super::*;

impl PackageLowerer<'_> {
    pub(super) fn lower_type(
        &mut self,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<TypeId, LowerError> {
        // Get type cache from `type_map`
        if let Some(ir_type) = self.type_map.get(&typ) {
            return Ok(*ir_type);
        }
        let underlying = self.analysis.underlying_type(typ);
        if let Some(ir_type) = self.type_map.get(&underlying).copied() {
            // Add cache for the named type
            self.type_map.insert(typ, ir_type);
            return Ok(ir_type);
        }
        if let Some(ir_type) = self.type_map.iter().find_map(|(semantic, ir_type)| {
            self.analysis
                .identical_types(underlying, *semantic)
                .then_some(*ir_type)
        }) {
            self.type_map.insert(underlying, ir_type);
            self.type_map.insert(typ, ir_type);
            return Ok(ir_type);
        }
        let result = match self.analysis.type_of(underlying).kind.clone() {
            TypeKind::Basic(BasicType::Bool) => self.builder.types().i1(),
            TypeKind::Basic(BasicType::Byte) => self.builder.types().i8(),
            TypeKind::Basic(BasicType::Int) => match self.target_width {
                32 => self.builder.types().i32(),
                64 => self.builder.types().i64(),
                _ => return Err(self.unsupported(node, "target pointer width")),
            },
            TypeKind::Pointer { base } => {
                let pointee = self.lower_type(base, node)?;
                self.pointer_type(pointee)
            }
            TypeKind::Array { len, elem } => {
                let result = self.builder.reserve_type();
                self.type_map.insert(underlying, result);
                self.type_map.insert(typ, result);
                let length = self.array_length(len, node)?;
                let element = self.lower_type(elem, node)?;
                self.builder
                    .define_type(result, IrTypeKind::Array { length, element })
                    .map_err(|source| LowerError::Build { node, source })?;
                return Ok(result);
            }
            TypeKind::Struct { fields } => {
                let result = self.builder.reserve_type();
                self.type_map.insert(underlying, result);
                self.type_map.insert(typ, result);
                let fields = fields
                    .into_iter()
                    .map(|field| self.lower_type(self.analysis.object(field).typ, node))
                    .collect::<Result<Vec<_>, _>>()?;
                self.builder
                    .define_type(result, IrTypeKind::Struct { fields })
                    .map_err(|source| LowerError::Build { node, source })?;
                return Ok(result);
            }
            _ => return Err(self.unsupported(node, "type")),
        };
        self.type_map.insert(underlying, result);
        self.type_map.insert(typ, result);
        Ok(result)
    }

    fn array_length(&self, length: ConstValue, node: AstNodeId) -> Result<u64, LowerError> {
        let ConstValue::Int(length) = length else {
            return Err(LowerError::InvalidConstant { node });
        };
        let length = length
            .to_i128()
            .and_then(|length| u64::try_from(length).ok())
            .filter(|length| *length != 0)
            .ok_or(LowerError::InvalidConstant { node })?;
        if self.target_width == 32 && length > u32::MAX as u64 {
            return Err(LowerError::InvalidConstant { node });
        }
        Ok(length)
    }

    pub(super) fn lower_constant(
        &self,
        constant: ConstValue,
        typ: gane_sema::TypeId,
        node: AstNodeId,
    ) -> Result<Constant, LowerError> {
        match constant {
            ConstValue::Bool(value) if self.analysis.is_basic_type(typ, BasicType::Bool) => {
                Ok(Constant::Bool(value))
            }
            ConstValue::Int(value) if self.analysis.is_basic_type(typ, BasicType::Byte) => value
                .to_i128()
                .filter(|value| (0..=u8::MAX as i128).contains(value))
                .map(|value| Constant::Integer(value as u64))
                .ok_or(LowerError::InvalidConstant { node }),
            ConstValue::Int(value) if self.analysis.is_basic_type(typ, BasicType::Int) => {
                let width = self.target_width as u32;
                let value = value
                    .to_i128()
                    .ok_or(LowerError::InvalidConstant { node })?;
                let minimum = -(1_i128 << (width - 1));
                let maximum = (1_i128 << (width - 1)) - 1;
                if !(minimum..=maximum).contains(&value) {
                    return Err(LowerError::InvalidConstant { node });
                }
                let mask = (1_u128 << width) - 1;
                Ok(Constant::Integer((value as u128 & mask) as u64))
            }
            _ => Err(LowerError::InvalidConstant { node }),
        }
    }

    pub(super) fn pointer_type(&mut self, pointee: TypeId) -> TypeId {
        if let Some(pointer) = self.pointer_types.get(&pointee) {
            return *pointer;
        }
        let pointer = self.builder.add_type(IrTypeKind::Ptr {
            pointee,
            address_space: 0,
        });
        self.pointer_types.insert(pointee, pointer);
        pointer
    }

    pub(super) fn pointer_integer_type(&self, node: AstNodeId) -> Result<TypeId, LowerError> {
        match self.target_width {
            32 => Ok(self.builder.types().i32()),
            64 => Ok(self.builder.types().i64()),
            _ => Err(self.unsupported(node, "target pointer width")),
        }
    }
}
