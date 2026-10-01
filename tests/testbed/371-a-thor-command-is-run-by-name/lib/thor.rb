class Thor
  def self.desc(usage, description)
    @usage = usage
  end

  def self.start(argv)
    new.public_send(argv.first)
  end
end
