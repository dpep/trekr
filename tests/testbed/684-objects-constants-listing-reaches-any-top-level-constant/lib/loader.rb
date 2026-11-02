class Loader
  def self.all
    Object.constants.map { |c| Object.const_get(c) }
  end
end
