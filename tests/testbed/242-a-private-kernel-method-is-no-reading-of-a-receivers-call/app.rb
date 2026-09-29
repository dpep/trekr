class Cref
  def autoload?
  end
end

class Loader
  def visit(cref)
    cref.autoload?
  end
end
