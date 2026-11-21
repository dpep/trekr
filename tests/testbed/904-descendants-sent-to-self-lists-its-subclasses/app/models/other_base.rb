class OtherBase
  def self.all_kinds
    T.unsafe(self).subclasses
  end
end
