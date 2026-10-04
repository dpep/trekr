module Store
  class SchemaDumper
    def dump
      tables
      extensions
    end

    def tables; end

    def extensions; end
  end
end
