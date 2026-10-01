class Thor
  class Group
    def self.start
      public_instance_methods(false).each { |m| new.public_send(m) }
    end
  end
end
