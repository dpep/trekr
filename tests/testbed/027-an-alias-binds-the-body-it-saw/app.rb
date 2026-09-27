class Gizmo
  def greet
    "a"
  end
  alias_method :old_greet, :greet
  alias first_greet greet

  def greet
    old_greet + first_greet + "b"
  end

  alias_method :inherited_hash, :hash
end
