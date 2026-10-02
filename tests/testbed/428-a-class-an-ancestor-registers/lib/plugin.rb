module Plugin
  ALL = []

  def self.included(base)
    ALL << base
  end
end
