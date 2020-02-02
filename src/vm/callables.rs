use std::fmt;
use std::collections::{HashMap};
use std::iter::FromIterator;

use vm::errors::{InterpreterResult as Result, Error};
use vm::analysis::errors::CheckErrors;
use vm::representations::{SymbolicExpression, ClarityName};
use vm::types::{TypeSignature, QualifiedContractIdentifier, TraitIdentifier, PrincipalData, FunctionType};
use vm::{eval, Value, LocalContext, Environment};
use vm::contexts::ContractContext;

pub enum CallableType {
    UserFunction(DefinedFunction),
    NativeFunction(&'static str, &'static dyn Fn(&[Value]) -> Result<Value>),
    SpecialFunction(&'static str, &'static dyn Fn(&[SymbolicExpression], &mut Environment, &LocalContext) -> Result<Value>)
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub enum DefineType {
    ReadOnly,
    Public,
    Private
}

#[derive(Clone,Serialize, Deserialize)]
pub struct DefinedFunction {
    identifier: FunctionIdentifier,
    name: ClarityName,
    arg_types: Vec<TypeSignature>,
    define_type: DefineType,
    arguments: Vec<ClarityName>,
    body: SymbolicExpression
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct FunctionIdentifier {
    identifier: String
}

impl fmt::Display for FunctionIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.identifier)
    }
}

impl DefinedFunction {
    pub fn new(mut arguments: Vec<(ClarityName, TypeSignature)>, 
               body: SymbolicExpression,
               define_type: DefineType, 
               name: &ClarityName, 
               context_name: &str) -> DefinedFunction {
        let (argument_names, types) = arguments.drain(..).unzip();

        DefinedFunction {
            identifier: FunctionIdentifier::new_user_function(name, context_name),
            name: name.clone(),
            arguments: argument_names,
            define_type,
            body,
            arg_types: types
        }
    }

    pub fn execute_apply(&self, args: &[Value], env: &mut Environment) -> Result<Value> {
        let mut context = LocalContext::new();
        if args.len() != self.arguments.len() {
            Err(CheckErrors::IncorrectArgumentCount(self.arguments.len(), args.len()))?
        }

        let mut arg_iterator: Vec<_> = self.arguments.iter().zip(self.arg_types.iter()).zip(args.iter()).collect();

        for arg in arg_iterator.drain(..) {
            let ((name, type_sig), value) = arg;
            match (type_sig, value) {
                (TypeSignature::TraitReferenceType(trait_reference), Value::Principal(PrincipalData::Contract(contract_id))) => {
                    // Argument is a trait reference, probably leading to a dynamic contract call
                    // This is the moment when we're making sure that the target contract is 
                    // conform.
                    let trait_identifier = env.contract_context.lookup_trait_reference(trait_reference).unwrap();
                    context.callable_contracts.insert(name.clone(), (contract_id.clone(), trait_identifier));
                },
                _ => {
                    if !type_sig.admits(value) {
                        return Err(CheckErrors::TypeValueError(type_sig.clone(), value.clone()).into())
                    }
                    if let Some(_) = context.variables.insert(name.clone(), value.clone()) {
                        return Err(CheckErrors::NameAlreadyUsed(name.to_string()).into())
                    }        
                }
            }
        }

        let result = eval(&self.body, env, &context);

        // if the error wasn't actually an error, but a function return,
        //    pull that out and return it.
        match result {
            Ok(r) => Ok(r),
            Err(e) => {
                match e {
                    Error::ShortReturn(v) => Ok(v.into()),
                    _ => Err(e)
                }
            }
        }
    }

    pub fn check_trait_expectations(&self, 
                                    contract_defining_trait: &ContractContext,
                                    trait_identifier: &TraitIdentifier, 
                                    contract_to_check: &ContractContext) -> Result<()> {

        let trait_name = trait_identifier.name.to_string();
        let constraining_trait = contract_defining_trait.lookup_trait_definition(&trait_name).unwrap();
        let expected_sig = constraining_trait.get(&self.name).unwrap();

        if expected_sig.args.len() != self.arg_types.len() {
            return Err(CheckErrors::BadTraitImplementation(trait_name.clone(), self.name.to_string()).into())
        }

        let args = expected_sig.args.iter().zip(self.arg_types.iter());
        for (expected_arg, actual_arg) in args {
            match (expected_arg, actual_arg) {
                (TypeSignature::TraitReferenceType(expected), TypeSignature::TraitReferenceType(ref actual)) => {
                    let expected_trait_id = contract_defining_trait.lookup_trait_reference(&expected.to_string())
                        .ok_or(CheckErrors::BadTraitImplementation(trait_name.clone(), self.name.to_string()))?;
                    let actual_trait_id = contract_to_check.lookup_trait_reference(&actual.to_string())
                        .ok_or(CheckErrors::BadTraitImplementation(trait_name.clone(), self.name.to_string()))?;
                    if actual_trait_id != expected_trait_id {
                        return Err(CheckErrors::BadTraitImplementation(trait_name.clone(), self.name.to_string()).into())
                    }
                }
                (_, arg_sig) => {
                    if !expected_arg.admits_type(&arg_sig) {
                        return Err(CheckErrors::BadTraitImplementation(trait_name.clone(), self.name.to_string()).into())
                    }        
                }
            }
        }
        Ok(())
    }

    pub fn is_read_only(&self) -> bool {
        self.define_type == DefineType::ReadOnly
    }

    pub fn apply(&self, args: &[Value], env: &mut Environment) -> Result<Value> {
        match self.define_type {
            DefineType::Private => self.execute_apply(args, env),
            DefineType::Public => env.execute_function_as_transaction(self, args, None),
            DefineType::ReadOnly => env.execute_function_as_transaction(self, args, None)
        }
    }

    pub fn is_public(&self) -> bool {
        match self.define_type {
            DefineType::Public => true,
            DefineType::Private => false,
            DefineType::ReadOnly => true
        }
    }

    pub fn get_identifier(&self) -> FunctionIdentifier {
        self.identifier.clone()
    }
}

impl CallableType {
    pub fn get_identifier(&self) -> FunctionIdentifier {
        match self {
            CallableType::UserFunction(f) => f.get_identifier(),
            CallableType::NativeFunction(s, _) => FunctionIdentifier::new_native_function(s),
            CallableType::SpecialFunction(s, _) => FunctionIdentifier::new_native_function(s),
        }
    }
}

impl FunctionIdentifier {
    fn new_native_function(name: &str) -> FunctionIdentifier {
        let identifier = format!("_native_:{}", name);
        FunctionIdentifier { identifier: identifier }
    }

    fn new_user_function(name: &str, context: &str) -> FunctionIdentifier {
        let identifier = format!("{}:{}", context, name);
        FunctionIdentifier { identifier: identifier }
    }
}
