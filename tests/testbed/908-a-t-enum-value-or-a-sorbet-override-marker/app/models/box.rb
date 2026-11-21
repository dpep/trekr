class Box
  extend T::Generic
  extend T::Sig
  Elem = type_member

  sig { override.void }
  def run
  end

  sig { overridable.void }
  def hook
  end

  def lonely
  end
end
