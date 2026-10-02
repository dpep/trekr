class Loader
  def load(name)
    Regions.const_get(name.capitalize)
  end
end
