module Jobs
  def self.enqueue(name)
    "Jobs::#{name.to_s.camelize}".constantize.new
  end
end
